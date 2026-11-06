//! Verify and sign off: run a check's local command on the pull request's
//! exact head commit, and write the signoff only when it passed.
//!
//! The order is the whole point:
//!
//! 1. Refuse unless the local checkout is at the pull request's head, so a
//!    command never vouches for a commit it did not run on.
//! 2. Run the user's mapped command in their login shell, streaming its
//!    output, stopped by its timeout or by the user.
//! 3. Only on exit code 0, check again that neither the checkout nor the
//!    pull request moved while it ran.
//! 4. Run `gh signoff --commit <sha> [check]`, naming the commit so the
//!    status lands on what was tested even if HEAD moves afterwards.
//!
//! Both the run and the signoff are recorded in local history.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use jiff::Timestamp;

use crate::checks::{CheckState, valid_context_name};
use crate::config::{ConfigStore, DEFAULT_TIMEOUT_SECONDS, MAX_TIMEOUT_SECONDS};
use crate::gh::{Gh, short_sha};
use crate::history::{ActivityEvent, ActivityKind, HistoryStore};
use crate::product::ProductService;
use crate::runner::Runner;

/// How long `gh signoff` may take.
pub const SIGNOFF_TIMEOUT: Duration = Duration::from_secs(120);

/// What to verify: one check of one pull request.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VerifyRequest {
    /// The tracked repository's id.
    pub repo_id: String,
    /// The pull request.
    pub pr_number: u64,
    /// The check's display name: `signoff` or `tests`.
    pub check: String,
}

impl VerifyRequest {
    /// One run at a time per pull request and check: gh-signoff is not
    /// reentrant per commit.
    fn key(&self) -> String {
        format!("{}:{}:{}", self.repo_id, self.pr_number, self.check)
    }
}

/// The step a run is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Confirming the checkout and pull request are at the expected commit.
    Checkout,
    /// Running the mapped command.
    Run,
    /// Writing the signoff.
    Signoff,
}

/// How a run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// The step it ended on.
    pub phase: Phase,
    /// The exit code of that step; 0 only when the signoff was written.
    pub code: i32,
    /// Why it stopped, when it did not succeed.
    pub message: Option<String>,
}

impl Outcome {
    /// Whether the signoff was written.
    pub fn signed_off(&self) -> bool {
        self.phase == Phase::Signoff && self.code == 0
    }

    fn stopped(phase: Phase, code: i32, message: impl Into<String>) -> Self {
        Self { phase, code, message: Some(message.into()) }
    }
}

/// What a run reports while it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerifyEvent {
    /// A new step started.
    Phase(Phase),
    /// A line of output, or a `$ command` echo before each process.
    Line(String),
}

/// Why a run did not start. Nothing ran and nothing was recorded.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// Not a name gh-signoff takes.
    #[error("{0:?} is not a valid check name")]
    InvalidCheck(String),
    /// The pull request is no longer in the latest state.
    #[error("this pull request is no longer tracked; refresh and try again")]
    PullRequestGone,
    /// The check is not one this pull request has.
    #[error("{0} is not a check on this pull request")]
    CheckMissing(String),
    /// No local command is mapped to the check.
    #[error("map a local command to {0} in Settings first")]
    NotConfigured(String),
    /// The repository was removed from the config.
    #[error("this repository is no longer tracked")]
    RepositoryGone,
    /// The same check of the same pull request is already running.
    #[error("{0} is already running for this pull request")]
    AlreadyRunning(String),
    /// The checkout is not at the pull request's head.
    #[error("{0}")]
    WrongCommit(String),
}

/// Runs checks and writes signoffs.
///
/// [`Verifier::verify`] blocks for as long as the command runs; call it from
/// a background thread.
pub struct Verifier {
    product: Arc<ProductService>,
    source: Source,
    running: Mutex<HashSet<String>>,
}

enum Source {
    /// Pretends, at `pace` per step, and never runs anything.
    Demo {
        pace: Duration,
    },
    Live {
        runner: Arc<dyn Runner>,
        gh: Gh,
        config: Arc<ConfigStore>,
        history: Arc<HistoryStore>,
    },
}

impl std::fmt::Debug for Verifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Verifier").field("demo", &matches!(self.source, Source::Demo { .. })).finish_non_exhaustive()
    }
}

/// Releases a [`VerifyRequest::key`] when a run ends, however it ends.
struct Running<'a> {
    set: &'a Mutex<HashSet<String>>,
    key: String,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.set.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.key);
    }
}

impl Verifier {
    /// Verifies the pull requests `product` shows, running processes through
    /// `runner` and recording into `history`.
    pub fn new(
        product: Arc<ProductService>,
        runner: Arc<dyn Runner>,
        config: Arc<ConfigStore>,
        history: Arc<HistoryStore>,
    ) -> Self {
        let gh = Gh::new(runner.clone());
        Self::from_source(product, Source::Live { runner, gh, config, history })
    }

    /// Plays a run on demo data: nothing is executed or written, and the
    /// check is marked passed in `product`.
    pub fn demo(product: Arc<ProductService>) -> Self {
        Self::from_source(product, Source::Demo { pace: Duration::from_millis(450) })
    }

    fn from_source(product: Arc<ProductService>, source: Source) -> Self {
        Self { product, source, running: Mutex::new(HashSet::new()) }
    }

    /// Whether `request` is running now.
    pub fn is_running(&self, request: &VerifyRequest) -> bool {
        self.running.lock().unwrap_or_else(PoisonError::into_inner).contains(&request.key())
    }

    /// Runs `request`'s command and, if it passes, signs off, reporting
    /// progress to `on_event`. Setting `cancel` stops the command.
    ///
    /// # Errors
    ///
    /// When the run cannot start. Once it has started, every ending,
    /// including a failed command, is an [`Outcome`].
    pub fn verify(
        &self,
        request: &VerifyRequest,
        cancel: &AtomicBool,
        on_event: &mut dyn FnMut(VerifyEvent),
    ) -> Result<Outcome, VerifyError> {
        let check = request.check.trim();
        if !valid_context_name(check) {
            return Err(VerifyError::InvalidCheck(request.check.clone()));
        }
        let pr =
            self.product.find_pull_request(&request.repo_id, request.pr_number).ok_or(VerifyError::PullRequestGone)?;
        let target =
            pr.checks.iter().find(|c| c.name == check).ok_or_else(|| VerifyError::CheckMissing(check.into()))?;
        if !target.configured || target.local_command.trim().is_empty() {
            return Err(VerifyError::NotConfigured(check.into()));
        }

        let _running = self.claim(request)?;
        let head = pr.pr.head_sha.clone();
        let mut ctx = Run { pr_title: &pr.pr.title, repo: &pr.repo, request, check, on_event };

        match &self.source {
            Source::Demo { pace } => Ok(self.play_demo(&mut ctx, &target.local_command, &head, *pace)),
            Source::Live { runner, gh, config, history } => {
                let repo = config.find_repository(&request.repo_id).ok_or(VerifyError::RepositoryGone)?;
                let mapping = repo
                    .commands
                    .get(check)
                    .filter(|m| !m.command.trim().is_empty())
                    .ok_or_else(|| VerifyError::NotConfigured(check.into()))?;
                gh.require_checked_out_commit(&repo.path, &head).map_err(VerifyError::WrongCommit)?;

                let timeout = command_timeout(mapping.timeout_seconds);
                let live = Live { runner: runner.as_ref(), gh, history, dir: &repo.path, repo_id: &repo.id };
                let outcome = live.run_and_sign_off(&mut ctx, mapping.command.trim(), timeout, &head, cancel);
                if outcome.signed_off() {
                    // Shown at once; the next read of the repository confirms it.
                    self.product.mark_signed_off(&repo.id, request.pr_number, check, &head);
                }
                Ok(outcome)
            }
        }
    }

    fn claim(&self, request: &VerifyRequest) -> Result<Running<'_>, VerifyError> {
        let key = request.key();
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if !running.insert(key.clone()) {
            return Err(VerifyError::AlreadyRunning(request.check.trim().to_owned()));
        }
        Ok(Running { set: &self.running, key })
    }

    fn play_demo(&self, ctx: &mut Run<'_>, command: &str, head: &str, pace: Duration) -> Outcome {
        ctx.emit(VerifyEvent::Phase(Phase::Run));
        ctx.line(format!("$ {command}"));
        thread::sleep(pace);
        ctx.line("local command passed");
        ctx.emit(VerifyEvent::Phase(Phase::Signoff));
        ctx.line(format!("$ {}", signoff_args(head, ctx.check).join(" ")));
        thread::sleep(pace);
        ctx.line(format!("signed off {} on {}", ctx.check, short_sha(head)));
        self.product.mark_demo_passed(&ctx.request.repo_id, ctx.request.pr_number, ctx.check);
        Outcome { phase: Phase::Signoff, code: 0, message: None }
    }
}

/// One run's target and where its events go.
struct Run<'a> {
    pr_title: &'a str,
    repo: &'a str,
    request: &'a VerifyRequest,
    check: &'a str,
    on_event: &'a mut dyn FnMut(VerifyEvent),
}

impl Run<'_> {
    fn emit(&mut self, event: VerifyEvent) {
        (self.on_event)(event);
    }

    fn line(&mut self, line: impl Into<String>) {
        self.emit(VerifyEvent::Line(line.into()));
    }

    fn activity(&self, kind: ActivityKind, repo_id: &str, command: String, code: i32, took: Duration) -> ActivityEvent {
        ActivityEvent {
            kind,
            repo_id: repo_id.to_owned(),
            repo: self.repo.to_owned(),
            pr_number: self.request.pr_number,
            pr_title: self.pr_title.to_owned(),
            check: self.check.to_owned(),
            state: if code == 0 { CheckState::Success } else { CheckState::Failure },
            actor: "you".into(),
            command,
            exit_code: code,
            duration_millis: i64::try_from(took.as_millis()).unwrap_or(i64::MAX),
            at: Timestamp::now(),
            ..ActivityEvent::default()
        }
    }
}

struct Live<'a> {
    runner: &'a dyn Runner,
    gh: &'a Gh,
    history: &'a HistoryStore,
    dir: &'a Path,
    repo_id: &'a str,
}

impl Live<'_> {
    fn run_and_sign_off(
        &self,
        ctx: &mut Run<'_>,
        command: &str,
        timeout: Duration,
        head: &str,
        cancel: &AtomicBool,
    ) -> Outcome {
        // The command.
        ctx.emit(VerifyEvent::Phase(Phase::Run));
        ctx.line(format!("$ {command}"));
        let started = Instant::now();
        let (program, args) = shell_invocation(command);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let code = match self.stream(ctx, &program, &args, timeout, cancel) {
            Ok(code) => code,
            Err(message) => {
                ctx.line(&message);
                return Outcome::stopped(Phase::Run, 1, message);
            }
        };
        self.record(ctx.activity(ActivityKind::Run, self.repo_id, command.to_owned(), code, started.elapsed()));
        if code != 0 {
            return Outcome::stopped(Phase::Run, code, run_failure(code, timeout));
        }
        if cancel.load(Ordering::Acquire) {
            return Outcome::stopped(Phase::Run, crate::runner::CANCELLED, "stopped before signing off");
        }

        // Nothing moved while it ran.
        ctx.emit(VerifyEvent::Phase(Phase::Checkout));
        if let Err(message) = self.gh.require_checked_out_commit(self.dir, head) {
            ctx.line(&message);
            return Outcome::stopped(Phase::Checkout, 1, message);
        }
        match self.gh.pull_request(self.dir, ctx.request.pr_number) {
            Ok(pr) if pr.head_sha != head => {
                let message = format!(
                    "the pull request moved to {} while the command ran; nothing was signed off",
                    short_sha(&pr.head_sha)
                );
                ctx.line(&message);
                return Outcome::stopped(Phase::Checkout, 1, message);
            }
            Ok(pr) if !pr.state.is_empty() && !pr.state.eq_ignore_ascii_case("OPEN") => {
                let message = format!("the pull request is {}; nothing was signed off", pr.state.to_lowercase());
                ctx.line(&message);
                return Outcome::stopped(Phase::Checkout, 1, message);
            }
            Ok(_) => {}
            Err(err) => {
                let message = format!("could not confirm the pull request's head: {err}");
                ctx.line(&message);
                return Outcome::stopped(Phase::Checkout, 1, message);
            }
        }

        // The signoff.
        ctx.emit(VerifyEvent::Phase(Phase::Signoff));
        let args = signoff_args(head, ctx.check);
        let shown = args.join(" ");
        ctx.line(format!("$ {shown}"));
        let started = Instant::now();
        let args: Vec<&str> = args[1..].iter().map(String::as_str).collect();
        // A signoff is quick and must not be left half done, so it ignores
        // the cancel button.
        let code = match self.stream(ctx, "gh", &args, SIGNOFF_TIMEOUT, &AtomicBool::new(false)) {
            Ok(code) => code,
            Err(message) => {
                ctx.line(&message);
                return Outcome::stopped(Phase::Signoff, 1, message);
            }
        };
        self.record(ctx.activity(ActivityKind::Signoff, self.repo_id, shown, code, started.elapsed()));
        if code == 0 {
            ctx.line(format!("signed off {} on {}", ctx.check, short_sha(head)));
            Outcome { phase: Phase::Signoff, code, message: None }
        } else {
            Outcome::stopped(Phase::Signoff, code, format!("gh signoff exited with {code}"))
        }
    }

    fn stream(
        &self,
        ctx: &mut Run<'_>,
        program: &str,
        args: &[&str],
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> Result<i32, String> {
        self.runner
            .stream(Some(self.dir), program, args, timeout, cancel, &mut |line| ctx.line(line))
            .map_err(|err| format!("could not start {program}: {err}"))
    }

    fn record(&self, event: ActivityEvent) {
        if let Err(err) = self.history.append(event) {
            log::warn!("could not record activity: {err}");
        }
    }
}

/// Why a command's exit code means it did not pass.
fn run_failure(code: i32, timeout: Duration) -> String {
    match code {
        crate::runner::TIMED_OUT => {
            format!("the command timed out after {}", crate::runner::human_duration(timeout))
        }
        crate::runner::CANCELLED => "stopped".to_owned(),
        code => format!("the command exited with {code}; nothing was signed off"),
    }
}

/// A mapping's timeout, defaulted and capped as the config does.
pub fn command_timeout(seconds: u32) -> Duration {
    let seconds = if seconds == 0 { DEFAULT_TIMEOUT_SECONDS } else { seconds.min(MAX_TIMEOUT_SECONDS) };
    Duration::from_secs(u64::from(seconds))
}

/// `gh signoff --commit <sha>`, plus the check unless it is the bare
/// `signoff` context. The check must already have passed
/// [`valid_context_name`], so it cannot be read as an option.
pub fn signoff_args(sha: &str, check: &str) -> Vec<String> {
    let mut args = vec!["gh".to_owned(), "signoff".to_owned(), "--commit".to_owned(), sha.to_owned()];
    if check != "signoff" {
        args.push(check.to_owned());
    }
    args
}

/// The program and arguments that run `command` in the user's shell.
///
/// On Unix that is their login shell (`$SHELL -l -c`), so a GUI app sees the
/// same PATH and tool versions as their terminal; `/bin/sh` when `$SHELL`
/// is unset. On Windows it is `cmd /D /S /C`.
pub fn shell_invocation(command: &str) -> (String, Vec<String>) {
    if cfg!(windows) {
        // `/S` strips exactly the outer quotes, leaving the command as typed.
        ("cmd".to_owned(), vec!["/D".into(), "/S".into(), "/C".into(), format!("\"{command}\"")])
    } else {
        let shell =
            std::env::var("SHELL").ok().filter(|s| Path::new(s).is_absolute()).unwrap_or_else(|| "/bin/sh".into());
        (shell, vec!["-l".into(), "-c".into(), command.to_owned()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::Check;
    use crate::config::{AppConfig, CommandMapping, TrackedRepository};
    use crate::gh::PrInfo;
    use crate::product::{ProductState, PullRequestState};
    use crate::runner::fake::{FakeRunner, Reply, ok};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    const SHA: &str = "3db401c9e8a1f00b5c2d7e6f4a3b2c1d0e9f8a7b";
    const OTHER: &str = "aaaaaaaa11111111222222223333333344444444";

    struct Fixture {
        verifier: Verifier,
        product: Arc<ProductService>,
        fake: Arc<FakeRunner>,
        history: Arc<HistoryStore>,
        _dir: tempfile::TempDir,
    }

    fn pr_view(sha: &'static str, state: &'static str) -> Reply {
        let json: &'static str = Box::leak(
            format!(r#"{{"number":7,"title":"Add retry","state":"{state}","headRefOid":"{sha}"}}"#).into_boxed_str(),
        );
        ok("pr view 7", json)
    }

    fn fixture(replies: Vec<Reply>) -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = TrackedRepository {
            id: "r1".into(),
            path: PathBuf::from("/work/app"),
            name_with_owner: "o/app".into(),
            enabled: true,
            commands: BTreeMap::from([(
                "tests".to_owned(),
                CommandMapping { command: "make test".into(), timeout_seconds: 60 },
            )]),
            ..TrackedRepository::default()
        };
        let config = Arc::new(ConfigStore::with_config(
            dir.path().join("config.json"),
            AppConfig { repositories: vec![repo], ..AppConfig::default() },
        ));
        let history = Arc::new(HistoryStore::open(dir.path().join("history.json")).expect("history"));
        let product = Arc::new(ProductService::with_state(ProductState {
            pull_requests: vec![PullRequestState {
                repo_id: "r1".into(),
                repo: "o/app".into(),
                pr: PrInfo { number: 7, title: "Add retry".into(), head_sha: SHA.into(), ..PrInfo::default() },
                checks: vec![
                    Check {
                        name: "tests".into(),
                        local_command: "make test".into(),
                        configured: true,
                        ..Check::default()
                    },
                    Check { name: "design".into(), ..Check::default() },
                ],
                ..PullRequestState::default()
            }],
            ..ProductState::default()
        }));
        let fake = Arc::new(FakeRunner::new(replies));
        let verifier = Verifier::new(product.clone(), fake.clone(), config, history.clone());
        Fixture { verifier, product, fake, history, _dir: dir }
    }

    fn request(check: &str) -> VerifyRequest {
        VerifyRequest { repo_id: "r1".into(), pr_number: 7, check: check.into() }
    }

    fn run(f: &Fixture, check: &str) -> (Result<Outcome, VerifyError>, Vec<VerifyEvent>) {
        let mut events = Vec::new();
        let result = f.verifier.verify(&request(check), &AtomicBool::new(false), &mut |e| events.push(e));
        (result, events)
    }

    fn lines(events: &[VerifyEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                VerifyEvent::Line(l) => Some(l.clone()),
                VerifyEvent::Phase(_) => None,
            })
            .collect()
    }

    #[test]
    fn signs_off_the_tested_commit_after_a_passing_run() {
        let f = fixture(vec![
            ok("rev-parse HEAD", SHA),
            Reply { matches: "make test", stdout: "ok 12 tests", stderr: "", code: 0 },
            pr_view(SHA, "OPEN"),
            ok("signoff --commit", "✓ Signed off"),
        ]);
        let (result, events) = run(&f, "tests");
        let outcome = result.expect("started");
        assert!(outcome.signed_off(), "{outcome:?}");
        assert!(lines(&events).contains(&"ok 12 tests".to_owned()));
        assert!(f.fake.called(&format!("gh signoff --commit {SHA} tests")));
        // The checkout is confirmed before and after the run.
        assert_eq!(f.fake.commands().iter().filter(|c| c.contains("rev-parse HEAD")).count(), 2);
        let phases: Vec<Phase> =
            events.iter().filter_map(|e| if let VerifyEvent::Phase(p) = e { Some(*p) } else { None }).collect();
        assert_eq!(phases, [Phase::Run, Phase::Checkout, Phase::Signoff]);

        let kinds: Vec<ActivityKind> = f.history.snapshot(0).iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&ActivityKind::Run) && kinds.contains(&ActivityKind::Signoff));
        assert!(f.history.snapshot(0).iter().all(|e| e.state == CheckState::Success && e.check == "tests"));

        let pr = f.product.find_pull_request("r1", 7).expect("pr");
        let tests = pr.checks.iter().find(|c| c.name == "tests").expect("check");
        assert_eq!(tests.state, CheckState::Success, "shown before GitHub is read again");
    }

    #[test]
    fn refuses_to_run_on_another_commit() {
        let f = fixture(vec![ok("rev-parse HEAD", OTHER)]);
        let (result, events) = run(&f, "tests");
        let Err(VerifyError::WrongCommit(message)) = result else { panic!("{result:?}") };
        assert!(message.contains("aaaaaaaa") && message.contains("3db401c9"), "{message}");
        assert!(events.is_empty());
        assert!(!f.fake.called("make test"));
        assert!(f.history.snapshot(0).is_empty());
    }

    #[test]
    fn a_failing_command_records_the_run_and_never_signs_off() {
        let f = fixture(vec![
            ok("rev-parse HEAD", SHA),
            Reply { matches: "make test", stdout: "", stderr: "1 failed", code: 2 },
        ]);
        let (result, _) = run(&f, "tests");
        let outcome = result.expect("started");
        assert_eq!((outcome.phase, outcome.code), (Phase::Run, 2));
        assert!(!f.fake.called("signoff"));
        let history = f.history.snapshot(0);
        assert_eq!(history.len(), 1);
        assert_eq!(
            (history[0].kind, history[0].state, history[0].exit_code),
            (ActivityKind::Run, CheckState::Failure, 2)
        );
        let pr = f.product.find_pull_request("r1", 7).expect("pr");
        assert!(pr.checks.iter().all(|c| c.state != CheckState::Success), "nothing shown as signed off");
        assert_eq!(history[0].command, "make test");
    }

    /// Someone pushed while the command ran: the checkout still matches, but
    /// GitHub's head does not, so the result no longer speaks for it.
    #[test]
    fn refuses_to_sign_off_when_the_pull_request_moved_during_the_run() {
        let f = fixture(vec![ok("rev-parse HEAD", SHA), ok("make test", ""), pr_view(OTHER, "OPEN")]);
        let (result, events) = run(&f, "tests");
        let outcome = result.expect("started");
        assert_eq!(outcome.phase, Phase::Checkout);
        assert!(lines(&events).iter().any(|l| l.contains("moved to aaaaaaaa")));
        assert!(!f.fake.called("signoff"));
    }

    #[test]
    fn refuses_to_sign_off_a_merged_pull_request() {
        let f = fixture(vec![ok("rev-parse HEAD", SHA), ok("make test", ""), pr_view(SHA, "MERGED")]);
        let (result, _) = run(&f, "tests");
        assert_eq!(result.expect("started").phase, Phase::Checkout);
        assert!(!f.fake.called("signoff"));
    }

    #[test]
    fn a_failed_signoff_is_recorded_as_one() {
        let f = fixture(vec![
            ok("rev-parse HEAD", SHA),
            ok("make test", ""),
            pr_view(SHA, "OPEN"),
            Reply { matches: "signoff --commit", stdout: "", stderr: "HTTP 403", code: 1 },
        ]);
        let (result, events) = run(&f, "tests");
        let outcome = result.expect("started");
        assert!(!outcome.signed_off());
        assert!(lines(&events).contains(&"HTTP 403".to_owned()));
        let signoff = f.history.snapshot(0).into_iter().find(|e| e.kind == ActivityKind::Signoff).expect("recorded");
        assert_eq!(signoff.state, CheckState::Failure);
        assert_eq!(signoff.command, format!("gh signoff --commit {SHA} tests"));
    }

    #[test]
    fn refuses_unconfigured_missing_and_unsafe_checks_before_running_anything() {
        let f = fixture(vec![]);
        assert_eq!(run(&f, "design").0, Err(VerifyError::NotConfigured("design".into())));
        assert_eq!(run(&f, "lint").0, Err(VerifyError::CheckMissing("lint".into())));
        assert_eq!(run(&f, "-f").0, Err(VerifyError::InvalidCheck("-f".into())));
        let gone = VerifyRequest { pr_number: 8, ..request("tests") };
        assert_eq!(f.verifier.verify(&gone, &AtomicBool::new(false), &mut |_| {}), Err(VerifyError::PullRequestGone));
        assert!(f.fake.commands().is_empty());
    }

    #[test]
    fn one_run_at_a_time_per_check() {
        let f = fixture(vec![]);
        let held = f.verifier.claim(&request("tests")).expect("first");
        assert!(f.verifier.is_running(&request("tests")));
        assert_eq!(run(&f, "tests").0, Err(VerifyError::AlreadyRunning("tests".into())));
        drop(held);
        assert!(!f.verifier.is_running(&request("tests")));
    }

    #[test]
    fn demo_runs_pretend_and_mark_the_check_passed() {
        let product = Arc::new(ProductService::demo());
        product.refresh();
        let pr = product.cached().pull_requests.into_iter().find(|pr| !pr.ready).expect("a pr to verify");
        let check =
            pr.checks.iter().find(|c| c.configured && c.state != CheckState::Success).expect("check").name.clone();
        let verifier = Verifier::from_source(product.clone(), Source::Demo { pace: Duration::ZERO });
        let request = VerifyRequest { repo_id: pr.repo_id.clone(), pr_number: pr.pr.number, check: check.clone() };
        let outcome = verifier.verify(&request, &AtomicBool::new(false), &mut |_| {}).expect("started");
        assert!(outcome.signed_off());
        let after = product.find_pull_request(&pr.repo_id, pr.pr.number).expect("pr");
        assert_eq!(after.checks.iter().find(|c| c.name == check).expect("check").state, CheckState::Success);
    }

    #[test]
    fn signoff_names_the_commit_and_omits_the_bare_context() {
        assert_eq!(signoff_args("abc", "signoff"), ["gh", "signoff", "--commit", "abc"]);
        assert_eq!(signoff_args("abc", "tests"), ["gh", "signoff", "--commit", "abc", "tests"]);
    }

    #[test]
    fn timeouts_default_and_cap_like_the_config() {
        assert_eq!(command_timeout(0), Duration::from_secs(u64::from(DEFAULT_TIMEOUT_SECONDS)));
        assert_eq!(command_timeout(90), Duration::from_secs(90));
        assert_eq!(command_timeout(u32::MAX), Duration::from_secs(u64::from(MAX_TIMEOUT_SECONDS)));
    }

    #[test]
    fn commands_run_in_a_shell() {
        let (program, args) = shell_invocation("make test && echo ok");
        if cfg!(windows) {
            assert_eq!(program, "cmd");
            assert_eq!(args.last().map(String::as_str), Some("\"make test && echo ok\""));
        } else {
            assert!(Path::new(&program).is_absolute());
            assert_eq!(args, ["-l", "-c", "make test && echo ok"]);
        }
    }

    /// A real process, end to end through the system runner: output streams
    /// and a non-zero exit stops before the signoff.
    #[cfg(unix)]
    #[test]
    fn a_real_command_streams_and_its_failure_stops_the_signoff() {
        use crate::runner::SystemRunner;
        struct GitOnly(SystemRunner);
        impl Runner for GitOnly {
            fn run(
                &self,
                _dir: Option<&Path>,
                _input: Option<&str>,
                program: &str,
                _args: &[&str],
            ) -> std::io::Result<crate::runner::Output> {
                assert_eq!(program, "git");
                Ok(crate::runner::Output { stdout: SHA.into(), ..Default::default() })
            }
            fn stream(
                &self,
                _dir: Option<&Path>,
                program: &str,
                args: &[&str],
                timeout: Duration,
                cancel: &AtomicBool,
                on_line: &mut dyn FnMut(&str),
            ) -> std::io::Result<i32> {
                assert_ne!(program, "gh", "must not sign off after a failure");
                // The user's shell may print a banner; run plain sh instead.
                let _ = args;
                self.0.stream(None, "sh", &["-c", "echo building; exit 3"], timeout, cancel, on_line)
            }
        }
        let f = fixture(vec![]);
        let Source::Live { config, history, .. } = &f.verifier.source else { unreachable!() };
        let verifier = Verifier::new(
            f.verifier.product.clone(),
            Arc::new(GitOnly(SystemRunner::default())),
            config.clone(),
            history.clone(),
        );
        let mut events = Vec::new();
        let outcome =
            verifier.verify(&request("tests"), &AtomicBool::new(false), &mut |e| events.push(e)).expect("started");
        assert_eq!((outcome.phase, outcome.code), (Phase::Run, 3));
        assert!(lines(&events).contains(&"building".to_owned()));
    }
}
