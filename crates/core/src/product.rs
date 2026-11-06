//! The whole state Signoff shows: every tracked repository's open pull
//! requests, their signoff checks, and recent activity.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::thread;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::checks::{
    self, Check, CheckState, Rules, display_name, is_signoff_context, latest_signoffs, resolve_checks, rules_for, tally,
};
use crate::config::{AppConfig, CommandMapping, ConfigStore, PullRequestScope, TrackedRepository};
use crate::gh::{CommitStatus, Environment, Gh, PrInfo, ProtectionStatus, RepoInfo, same_contexts};
use crate::history::{ActivityEvent, ActivityKind, HistoryStore};

/// Repositories refreshed at once.
const PARALLEL_REPOSITORIES: usize = 4;
/// Activity entries kept in a [`ProductState`].
const MAX_HISTORY: usize = 300;

/// How a pull request, or everything, is doing. The tray icon is coloured by
/// the worst one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Health {
    /// Nothing to report: no required checks, or no pull requests.
    #[default]
    Neutral,
    /// Every required check passed.
    Ready,
    /// Required checks still to sign off.
    Attention,
    /// A required check failed, or the pull request could not be read.
    Failing,
}

impl Health {
    /// Sort order for lists: failing first, ready last.
    pub fn rank(self) -> u8 {
        match self {
            Self::Failing => 0,
            Self::Attention => 1,
            Self::Neutral => 2,
            Self::Ready => 3,
        }
    }
}

/// Counts across every pull request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProductSummary {
    /// Pull requests shown.
    pub total_pull_requests: usize,
    /// Failing or needing attention.
    pub needs_attention: usize,
    /// Ready to merge.
    pub ready: usize,
    /// Failing.
    pub failing: usize,
    /// The overall health.
    pub status: Health,
}

/// One tracked repository, as last refreshed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RepositoryState {
    /// The tracked repository's id.
    pub id: String,
    /// The checkout.
    pub path: PathBuf,
    /// `owner/name`.
    pub name_with_owner: String,
    /// Its default branch.
    pub default_branch: String,
    /// The branch checked out locally.
    pub current_branch: String,
    /// The commit checked out locally.
    pub current_sha: String,
    /// Whether it is refreshed.
    pub enabled: bool,
    /// Whose pull requests it shows.
    pub pull_request_scope: PullRequestScope,
    /// Local commands by check name.
    pub commands: BTreeMap<String, CommandMapping>,
    /// Open pull requests shown.
    pub pull_request_count: usize,
    /// More were open than are shown.
    pub pull_requests_truncated: bool,
    /// Why it could not be read.
    pub error: Option<String>,
}

/// One open pull request and its signoff checks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PullRequestState {
    /// `<repo id>:<number>`.
    pub id: String,
    /// The tracked repository's id.
    pub repo_id: String,
    /// `owner/name`.
    pub repo: String,
    /// The checkout.
    pub repo_path: PathBuf,
    /// The pull request.
    pub pr: PrInfo,
    /// Its signoff checks, required first.
    pub checks: Vec<Check>,
    /// What protection on its base branch requires.
    pub rules: Rules,
    /// Required checks that passed.
    pub passed: usize,
    /// Required checks.
    pub total: usize,
    /// Every required check passed.
    pub ready: bool,
    /// The first required check that has not.
    pub blocking: Option<String>,
    /// How it is doing.
    pub status: Health,
    /// Its head commit is checked out locally, so checks can run.
    pub checked_out: bool,
    /// A caveat to show.
    pub warning: Option<String>,
    /// Why it could not be read.
    pub error: Option<String>,
    /// When it was read.
    pub fetched_at: Timestamp,
}

impl PullRequestState {
    /// Recounts after a check changed.
    pub fn retally(&mut self) {
        let t = tally(&self.checks);
        self.passed = t.passed;
        self.total = t.total;
        self.blocking = t.blocking;
        self.ready = t.ready;
        self.status = pull_request_health(self);
    }
}

/// Everything Signoff shows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProductState {
    /// What is installed and signed in.
    pub env: Environment,
    /// Counts and overall health.
    pub summary: ProductSummary,
    /// Every tracked repository.
    pub repositories: Vec<RepositoryState>,
    /// Open pull requests, failing first.
    pub pull_requests: Vec<PullRequestState>,
    /// GitHub statuses and local activity, newest first.
    pub history: Vec<ActivityEvent>,
    /// The config file.
    pub config_path: PathBuf,
    /// At least one repository is tracked.
    pub onboarded: bool,
    /// This is demo data.
    pub demo: bool,
    /// A refresh is running.
    pub refreshing: bool,
    /// A caveat to show.
    pub warning: Option<String>,
    /// What went wrong.
    pub error: Option<String>,
    /// When it was read; `None` before the first refresh.
    pub fetched_at: Option<Timestamp>,
}

impl ProductState {
    /// The pull request with `number` in the repository with `repo_id`.
    pub fn pull_request(&self, repo_id: &str, number: u64) -> Option<&PullRequestState> {
        self.pull_requests.iter().find(|pr| pr.repo_id == repo_id && pr.pr.number == number)
    }
}

/// How a pull request is doing.
pub fn pull_request_health(pr: &PullRequestState) -> Health {
    if pr.error.is_some() || pr.checks.iter().any(|c| c.required && c.state.is_failed()) {
        Health::Failing
    } else if pr.ready {
        Health::Ready
    } else if pr.total > 0 {
        Health::Attention
    } else {
        Health::Neutral
    }
}

/// Counts across pull requests; the overall health is the worst one.
pub fn summarize(prs: &[PullRequestState]) -> ProductSummary {
    let mut summary = ProductSummary { total_pull_requests: prs.len(), ..ProductSummary::default() };
    for pr in prs {
        match pr.status {
            Health::Failing => {
                summary.failing += 1;
                summary.needs_attention += 1;
            }
            Health::Attention => summary.needs_attention += 1,
            Health::Ready => summary.ready += 1,
            Health::Neutral => {}
        }
    }
    summary.status = if summary.failing > 0 {
        Health::Failing
    } else if summary.needs_attention > 0 {
        Health::Attention
    } else if summary.total_pull_requests > 0 && summary.ready == summary.total_pull_requests {
        Health::Ready
    } else {
        Health::Neutral
    };
    summary
}

/// Detailed statuses per pull-request head.
///
/// The pull-request list already reports each context and its state, so a
/// refresh needs no per-pull-request call to know what is green. What the
/// list omits is who posted it, which the history wants. Caching that by head
/// commit means attribution is fetched once when a signoff appears, not once
/// per pull request on every poll.
#[derive(Debug, Default)]
pub struct StatusCache {
    entries: Mutex<HashMap<String, CacheEntry>>,
}

#[derive(Clone, Debug)]
struct CacheEntry {
    rollup: Vec<CommitStatus>,
    detailed: Vec<CommitStatus>,
}

impl StatusCache {
    /// Detailed statuses for `repo@sha`, calling `fetch` only when `rollup`
    /// shows something the cache has not seen. A failed fetch falls back to
    /// the rollup: attribution is a nicety, and the rollup carries state.
    pub fn resolve<E>(
        &self,
        repo: &str,
        sha: &str,
        rollup: &[CommitStatus],
        fetch: impl FnOnce() -> Result<Vec<CommitStatus>, E>,
    ) -> Vec<CommitStatus> {
        let key = format!("{repo}@{sha}");
        if let Some(entry) = self.lock().get(&key)
            && same_contexts(&entry.rollup, rollup)
        {
            return entry.detailed.clone();
        }
        let Ok(detailed) = fetch() else {
            return rollup.to_vec();
        };
        let mut entries = self.lock();
        if entries.len() >= 512 {
            entries.clear();
        }
        entries.insert(key, CacheEntry { rollup: rollup.to_vec(), detailed: detailed.clone() });
        detailed
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, CacheEntry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Reads everything for `config` from GitHub. Repositories are read a few at
/// a time; this blocks for as long as that takes.
pub fn build_product_state(
    gh: &Gh,
    config: &AppConfig,
    local_history: Vec<ActivityEvent>,
    config_path: &Path,
    seed_dir: Option<&Path>,
    cache: &StatusCache,
) -> ProductState {
    let mut state = ProductState {
        config_path: config_path.to_owned(),
        onboarded: !config.repositories.is_empty(),
        fetched_at: Some(Timestamp::now()),
        ..ProductState::default()
    };
    let probe_dir = config.repositories.iter().find(|r| r.enabled).map(|r| r.path.as_path()).or(seed_dir);
    state.env = gh.probe_environment(probe_dir);
    if !state.env.gh_installed || !state.env.authenticated {
        state.history = local_history;
        return state;
    }

    let repositories = &config.repositories;
    let results: Mutex<Vec<Option<RepositoryResult>>> = Mutex::new(vec![None; repositories.len()]);
    let next = AtomicUsize::new(0);
    thread::scope(|scope| {
        for _ in 0..PARALLEL_REPOSITORIES.min(repositories.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(tracked) = repositories.get(index) else { break };
                    let result = build_repository_state(gh, tracked, cache);
                    results.lock().unwrap_or_else(PoisonError::into_inner)[index] = Some(result);
                }
            });
        }
    });

    for result in results.into_inner().unwrap_or_else(PoisonError::into_inner).into_iter().flatten() {
        state.repositories.push(result.repository);
        state.pull_requests.extend(result.pull_requests);
        state.history.extend(result.history);
    }
    state.history.extend(local_history);
    state.history.sort_by_key(|e| std::cmp::Reverse(e.at));
    state.history.truncate(MAX_HISTORY);
    state
        .pull_requests
        .sort_by(|a, b| a.status.rank().cmp(&b.status.rank()).then_with(|| b.pr.updated_at.cmp(&a.pr.updated_at)));
    state.summary = summarize(&state.pull_requests);
    state
}

#[derive(Clone, Debug)]
struct RepositoryResult {
    repository: RepositoryState,
    pull_requests: Vec<PullRequestState>,
    history: Vec<ActivityEvent>,
}

fn build_repository_state(gh: &Gh, tracked: &TrackedRepository, cache: &StatusCache) -> RepositoryResult {
    let mut view = RepositoryState {
        id: tracked.id.clone(),
        path: tracked.path.clone(),
        name_with_owner: tracked.name_with_owner.clone(),
        default_branch: tracked.default_branch.clone(),
        enabled: tracked.enabled,
        pull_request_scope: tracked.pull_request_scope,
        commands: tracked.commands.clone(),
        ..RepositoryState::default()
    };
    let empty = |repository| RepositoryResult { repository, pull_requests: Vec::new(), history: Vec::new() };
    if !tracked.enabled {
        return empty(view);
    }
    let info = match gh.repo(&tracked.path) {
        Ok(info) => info,
        Err(err) => {
            view.error = Some(err.to_string());
            return empty(view);
        }
    };
    view.name_with_owner.clone_from(&info.name_with_owner);
    view.default_branch.clone_from(&info.default_branch);
    view.current_branch.clone_from(&info.current_branch);
    view.current_sha.clone_from(&info.head_sha);

    let (prs, truncated) = match gh.pull_requests(&tracked.path, tracked.pull_request_scope == PullRequestScope::All) {
        Ok(found) => found,
        Err(err) => {
            view.error = Some(err.to_string());
            return empty(view);
        }
    };
    view.pull_request_count = prs.len();
    view.pull_requests_truncated = truncated;

    let mut protection: HashMap<String, (Vec<String>, ProtectionStatus)> = HashMap::new();
    let mut pull_requests = Vec::with_capacity(prs.len());
    let mut history = Vec::new();
    for pr in prs {
        let (contexts, status) = protection
            .entry(pr.base_ref.clone())
            .or_insert_with(|| gh.protection_contexts(&tracked.path, &info.name_with_owner, &pr.base_ref))
            .clone();
        let (state, events) = build_pull_request_state(gh, tracked, &info, pr, contexts, status, cache);
        pull_requests.push(state);
        history.extend(events);
    }
    RepositoryResult { repository: view, pull_requests, history }
}

fn build_pull_request_state(
    gh: &Gh,
    tracked: &TrackedRepository,
    repo: &RepoInfo,
    pr: PrInfo,
    contexts: Vec<String>,
    protection: ProtectionStatus,
    cache: &StatusCache,
) -> (PullRequestState, Vec<ActivityEvent>) {
    let rules = rules_for(&pr.base_ref, contexts, protection);
    // State comes from the rollup on the pull-request list, at no extra
    // call; the cache reaches for the status API only when something changed.
    let statuses = cache.resolve(&repo.name_with_owner, &pr.head_sha, &pr.contexts, || {
        gh.commit_statuses(&tracked.path, &repo.name_with_owner, &pr.head_sha)
    });

    let mut checks = resolve_checks(&rules.signoff_contexts, &latest_signoffs(&statuses), rules.readable);
    for check in &mut checks {
        if let Some(mapping) = tracked.commands.get(&check.name).filter(|m| !m.command.trim().is_empty()) {
            check.local_command.clone_from(&mapping.command);
            check.configured = true;
        }
    }

    let history = statuses
        .iter()
        .filter(|s| is_signoff_context(&s.context))
        .map(|s| ActivityEvent {
            id: format!("github:{}:{}:{}:{}", tracked.id, pr.number, s.context, s.created_at),
            kind: ActivityKind::Status,
            repo_id: tracked.id.clone(),
            repo: repo.name_with_owner.clone(),
            pr_number: pr.number,
            pr_title: pr.title.clone(),
            check: display_name(&s.context).to_owned(),
            state: s.state,
            actor: checks::actor_from(s),
            detail: s.description.clone(),
            at: s.created_at,
            ..ActivityEvent::default()
        })
        .collect();

    let mut state = PullRequestState {
        id: format!("{}:{}", tracked.id, pr.number),
        repo_id: tracked.id.clone(),
        repo: repo.name_with_owner.clone(),
        repo_path: tracked.path.clone(),
        checked_out: !repo.head_sha.is_empty() && repo.head_sha == pr.head_sha,
        warning: (!rules.readable)
            .then(|| "Branch protection is unreadable; readiness uses the default signoff assumption.".to_owned()),
        fetched_at: Timestamp::now(),
        pr,
        checks,
        rules,
        ..PullRequestState::default()
    };
    state.retally();
    (state, history)
}

enum Source {
    GitHub {
        gh: Gh,
        config: Arc<ConfigStore>,
        history: Arc<HistoryStore>,
        seed_dir: Option<PathBuf>,
        cache: StatusCache,
    },
    Demo,
}

/// Keeps the latest [`ProductState`] and refreshes it.
///
/// [`ProductService::refresh`] blocks on `gh`; call it from a background
/// thread and read [`ProductService::cached`] from the UI.
pub struct ProductService {
    source: Source,
    state: RwLock<ProductState>,
    refreshing: Mutex<()>,
}

impl std::fmt::Debug for ProductService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProductService").field("demo", &self.is_demo()).finish_non_exhaustive()
    }
}

impl ProductService {
    /// Reads GitHub for the repositories in `config`. `seed_dir` is probed
    /// for the environment when no repository is tracked yet.
    pub fn new(gh: Gh, config: Arc<ConfigStore>, history: Arc<HistoryStore>, seed_dir: Option<PathBuf>) -> Self {
        Self::from_source(Source::GitHub { gh, config, history, seed_dir, cache: StatusCache::default() })
    }

    /// Serves [`crate::demo::product_state`] and never touches GitHub or disk.
    pub fn demo() -> Self {
        Self::from_source(Source::Demo)
    }

    fn from_source(source: Source) -> Self {
        Self { source, state: RwLock::new(ProductState::default()), refreshing: Mutex::new(()) }
    }

    /// Serves `state` as if just refreshed, for tests elsewhere in the crate.
    #[cfg(test)]
    pub(crate) fn with_state(state: ProductState) -> Self {
        let service = Self::demo();
        *service.state.write().unwrap_or_else(PoisonError::into_inner) = state;
        service
    }

    /// Whether this serves demo data.
    pub fn is_demo(&self) -> bool {
        matches!(self.source, Source::Demo)
    }

    /// The config store, outside demo mode.
    pub fn config(&self) -> Option<&Arc<ConfigStore>> {
        match &self.source {
            Source::GitHub { config, .. } => Some(config),
            Source::Demo => None,
        }
    }

    /// The latest state, without reading anything.
    pub fn cached(&self) -> ProductState {
        self.state.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Reads everything again and returns the new state. Concurrent calls
    /// queue rather than overlap.
    pub fn refresh(&self) -> ProductState {
        let _one_at_a_time = self.refreshing.lock().unwrap_or_else(PoisonError::into_inner);
        self.state.write().unwrap_or_else(PoisonError::into_inner).refreshing = true;

        let mut next = match &self.source {
            // Demo data only changes through demo runs; a refresh must not
            // undo them.
            Source::Demo => {
                let current = self.cached();
                if current.fetched_at.is_some() { current } else { crate::demo::product_state(Timestamp::now()) }
            }
            Source::GitHub { gh, config, history, seed_dir, cache } => {
                let config_path = config.path().to_owned();
                build_product_state(
                    gh,
                    &config.snapshot(),
                    history.snapshot(200),
                    &config_path,
                    seed_dir.as_deref(),
                    cache,
                )
            }
        };
        next.refreshing = false;
        *self.state.write().unwrap_or_else(PoisonError::into_inner) = next.clone();
        next
    }

    /// The pull request with `number` in the repository with `repo_id`.
    pub fn find_pull_request(&self, repo_id: &str, number: u64) -> Option<PullRequestState> {
        self.state.read().unwrap_or_else(PoisonError::into_inner).pull_request(repo_id, number).cloned()
    }

    /// In demo mode, marks a check passed, as a successful run would. Does
    /// nothing outside demo mode.
    pub fn mark_demo_passed(&self, repo_id: &str, number: u64, check_name: &str) {
        if !self.is_demo() {
            return;
        }
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        let now = Timestamp::now();
        let Some(pr) = state.pull_requests.iter_mut().find(|pr| pr.repo_id == repo_id && pr.pr.number == number) else {
            return;
        };
        for check in pr.checks.iter_mut().filter(|c| c.name == check_name) {
            check.state = CheckState::Success;
            check.actor = "you".into();
            check.at = Some(now);
        }
        pr.retally();
        let event = ActivityEvent {
            id: format!("demo-run:{}", now.as_nanosecond()),
            kind: ActivityKind::Run,
            repo_id: repo_id.to_owned(),
            repo: pr.repo.clone(),
            pr_number: number,
            pr_title: pr.pr.title.clone(),
            check: check_name.to_owned(),
            state: CheckState::Success,
            actor: "you".into(),
            at: now,
            ..ActivityEvent::default()
        };
        state.history.insert(0, event);
        state.summary = summarize(&state.pull_requests);
        state.fetched_at = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::fake::{FakeRunner, fail, ok};

    #[test]
    fn summary_uses_the_worst_pull_request() {
        let prs: Vec<PullRequestState> = [Health::Ready, Health::Attention, Health::Failing]
            .into_iter()
            .map(|status| PullRequestState { status, ..PullRequestState::default() })
            .collect();
        let s = summarize(&prs);
        assert_eq!((s.total_pull_requests, s.ready, s.needs_attention, s.failing), (3, 1, 2, 1));
        assert_eq!(s.status, Health::Failing);
        assert_eq!(summarize(&[]).status, Health::Neutral);
    }

    #[test]
    fn status_cache_only_fetches_when_contexts_change() {
        let cache = StatusCache::default();
        let rollup = vec![CommitStatus {
            context: "signoff/tests".into(),
            state: CheckState::Success,
            ..CommitStatus::default()
        }];
        let detailed = vec![CommitStatus { creator: "octocat".into(), ..rollup[0].clone() }];
        let mut fetches = 0;
        for _ in 0..3 {
            let got = cache.resolve("o/r", "abc", &rollup, || {
                fetches += 1;
                Ok::<_, ()>(detailed.clone())
            });
            assert_eq!(got[0].creator, "octocat");
        }
        assert_eq!(fetches, 1);

        let changed = vec![CommitStatus { state: CheckState::Failure, ..rollup[0].clone() }];
        cache.resolve("o/r", "abc", &changed, || {
            fetches += 1;
            Ok::<_, ()>(changed.clone())
        });
        assert_eq!(fetches, 2);
    }

    #[test]
    fn status_cache_falls_back_to_the_rollup_on_error() {
        let rollup = vec![CommitStatus {
            context: "signoff/tests".into(),
            state: CheckState::Success,
            ..CommitStatus::default()
        }];
        let got = StatusCache::default().resolve("o/r", "abc", &rollup, || Err("network down"));
        assert_eq!(got, rollup);
    }

    #[test]
    fn demo_run_makes_the_pull_request_ready() {
        let service = ProductService::demo();
        service.refresh();
        assert!(!service.find_pull_request("demo-signoff", 482).expect("pr").ready);
        service.mark_demo_passed("demo-signoff", 482, "security");
        let pr = service.find_pull_request("demo-signoff", 482).expect("pr");
        assert!(pr.ready);
        assert_eq!(pr.status, Health::Ready);
        assert_eq!(service.cached().history[0].kind, ActivityKind::Run);
        assert!(service.refresh().pull_request("demo-signoff", 482).expect("pr").ready, "kept across refresh");
    }

    /// One repository end to end against recorded `gh` output: a failing
    /// pull request sorts first and attribution comes from the status API.
    #[test]
    fn builds_state_from_gh() {
        let fake = Arc::new(FakeRunner::new(vec![
            ok("gh --version", "gh version 2.95.0"),
            ok("api user", r#"{"login":"you"}"#),
            ok("rev-parse --git-dir", ".git"),
            ok("signoff version", "gh-signoff 0.3.0"),
            ok("repo view", r#"{"nameWithOwner":"o/r","defaultBranchRef":{"name":"main"}}"#),
            ok("rev-parse --abbrev-ref", "feature"),
            ok("rev-parse HEAD", "aaa"),
            ok(
                "pr list",
                r#"[
                {"number":1,"title":"Ready one","headRefOid":"aaa","baseRefName":"main","updatedAt":"2026-08-10T08:00:00Z",
                 "statusCheckRollup":[{"__typename":"StatusContext","context":"signoff/tests","state":"SUCCESS","startedAt":"2026-08-10T08:00:00Z"}]},
                {"number":2,"title":"Failing one","headRefOid":"bbb","baseRefName":"main","updatedAt":"2026-08-09T08:00:00Z",
                 "statusCheckRollup":[{"__typename":"StatusContext","context":"signoff/tests","state":"FAILURE","startedAt":"2026-08-09T08:00:00Z"}]}
            ]"#,
            ),
            ok("protection", r#"["signoff/tests","ci/build"]"#),
            ok(
                "commits/aaa",
                r#"{"statuses":[{"context":"signoff/tests","state":"success","description":"you signed off","created_at":"2026-08-10T08:00:00Z","creator":null}]}"#,
            ),
            fail("commits/bbb", "boom"),
        ]));
        let config = AppConfig {
            repositories: vec![TrackedRepository {
                id: "r1".into(),
                path: "/repo".into(),
                name_with_owner: "o/r".into(),
                enabled: true,
                commands: BTreeMap::from([(
                    "tests".into(),
                    CommandMapping { command: "cargo test".into(), timeout_seconds: 60 },
                )]),
                ..TrackedRepository::default()
            }],
            ..AppConfig::default()
        };
        let state = build_product_state(
            &Gh::new(fake.clone()),
            &config,
            Vec::new(),
            Path::new("/c/config.json"),
            None,
            &StatusCache::default(),
        );

        assert!(state.onboarded);
        assert_eq!(state.pull_requests.len(), 2);
        assert_eq!(state.pull_requests[0].pr.number, 2, "failing sorts first");
        assert_eq!(state.pull_requests[0].status, Health::Failing);
        let ready = &state.pull_requests[1];
        assert_eq!(ready.status, Health::Ready);
        assert!(ready.checked_out);
        assert!(ready.checks[0].configured);
        assert_eq!(ready.checks[0].actor, "you");
        assert_eq!(ready.rules.signoff_contexts, ["signoff/tests"]);
        assert_eq!(state.summary.status, Health::Failing);
        assert_eq!(
            fake.commands().iter().filter(|c| c.contains("protection")).count(),
            1,
            "protection is read once per base branch"
        );
    }

    #[test]
    fn signed_out_skips_repositories() {
        let fake = Arc::new(FakeRunner::new(vec![
            ok("gh --version", "gh version 2.95.0"),
            fail("api user", "not logged in"),
            fail("rev-parse", "not a repo"),
            fail("signoff version", "unknown"),
        ]));
        let config = AppConfig {
            repositories: vec![TrackedRepository {
                id: "r1".into(),
                path: "/repo".into(),
                enabled: true,
                ..TrackedRepository::default()
            }],
            ..AppConfig::default()
        };
        let state = build_product_state(
            &Gh::new(fake),
            &config,
            Vec::new(),
            Path::new("/c/config.json"),
            None,
            &StatusCache::default(),
        );
        assert!(!state.env.authenticated);
        assert!(state.repositories.is_empty());
    }
}
