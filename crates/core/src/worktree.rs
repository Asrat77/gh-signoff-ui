//! Worktrees Signoff manages, so a pull request can be verified without
//! touching the person's own checkout.
//!
//! With the setting on, a run whose checkout is not at the pull request's
//! head runs in `<root>/<repository id>` instead: a linked `git worktree`
//! of the tracked checkout, detached at the head commit. There is one per
//! repository, reused from run to run so build caches survive, and only
//! one run uses it at a time ([`Worktrees::claim`]).
//!
//! What preparing one changes in the tracked repository is its worktree
//! entry (`.git/worktrees/<id>`, which holds the worktree's own `HEAD`
//! and index) and the objects fetched. Never a branch, nor the checkout's
//! `HEAD`, index or files: the git watcher, which watches only those, sees
//! nothing.
//!
//! Deleting is fenced in: only a real folder (never a link) directly
//! inside the root and named like a repository id is ever removed, and
//! the root is never inside a tracked checkout.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::config::{AppConfig, TrackedRepository};
use crate::gh::short_sha;
use crate::git;
use crate::runner::{CANCELLED, Runner, TIMED_OUT, human_duration};

/// How long one `git` step of preparing a worktree may take: a first fetch
/// or checkout of a big repository is slow.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(600);

/// The worktrees under one root folder.
#[derive(Clone, Debug)]
pub struct Worktrees {
    root: PathBuf,
    /// Repository ids whose worktree a run or a removal holds.
    in_use: Arc<Mutex<HashSet<String>>>,
}

/// Holds one repository's worktree: no other run prepares it and no
/// cleanup removes it until this is dropped.
#[derive(Debug)]
pub struct Lease {
    worktrees: Worktrees,
    repo_id: String,
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.worktrees.in_use.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.repo_id);
    }
}

impl Lease {
    /// Makes the held worktree, `repo`'s, hold exactly `head`, pull request `number`'s
    /// head commit: creates or reuses it, fetches the commit, checks it out
    /// detached, drops stray changes and confirms. Each `git` command and
    /// its output goes to `on_line`; `cancel` stops a fetch or checkout.
    ///
    /// # Errors
    ///
    /// What failed and how to fix it.
    pub(crate) fn prepare(
        &self,
        runner: &dyn Runner,
        repo: &TrackedRepository,
        number: u64,
        head: &str,
        cancel: &AtomicBool,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<PathBuf, PrepareError> {
        if self.repo_id != repo.id {
            return Err(PrepareError::new("this run does not hold the repository's worktree"));
        }
        if !valid_commit(head) {
            return Err(PrepareError::new(format!("{head:?} is not a commit id")));
        }
        let wt = self
            .worktrees
            .path_for(&repo.id)
            .ok_or_else(|| PrepareError::new(format!("{:?} cannot name a worktree folder", repo.id)))?;
        let checkout = fs::canonicalize(&repo.path).map_err(|_| {
            PrepareError::new(format!(
                "{} is not there any more; add the checkout again in Settings",
                repo.path.display()
            ))
        })?;
        fs::create_dir_all(&self.worktrees.root).map_err(|err| {
            PrepareError::new(format!(
                "could not create {}: {err}; check that the folder is writable",
                self.worktrees.root.display()
            ))
        })?;
        let root = fs::canonicalize(&self.worktrees.root)
            .map_err(|err| PrepareError::new(format!("could not read {}: {err}", self.worktrees.root.display())))?;
        if root.starts_with(&checkout) || checkout.starts_with(&root) {
            return Err(PrepareError::new(format!(
                "Signoff's worktree folder {} overlaps the checkout {}; nothing was changed",
                root.display(),
                checkout.display()
            )));
        }
        let common = git::watch_paths(&repo.path)
            .map(|paths| paths.common_dir)
            .ok_or_else(|| PrepareError::new(format!("{} is not a git checkout", repo.path.display())))?;
        let mut git = Git { runner, cancel, on_line };

        if !reusable(&wt, &common) {
            if fs::symlink_metadata(&wt).is_ok() {
                self.worktrees.remove_unclaimed(runner, &wt).map_err(PrepareError::new)?;
            }
            // A worktree whose folder was deleted is still registered, and
            // `worktree add` refuses its path until it is pruned.
            git.quiet(&repo.path, &["worktree", "prune"]);
            let target = wt.to_string_lossy();
            git.stream(&repo.path, &["worktree", "add", "--detach", "--no-checkout", &target]).map_err(|failed| {
                failed.explain(format!(
                    "could not create a worktree in {}: {}; check that `git worktree add` works in {}",
                    wt.display(),
                    failed.detail,
                    repo.path.display()
                ))
            })?;
        }

        let remote = pick_remote(&git.quiet(&repo.path, &["remote", "-v"]).unwrap_or_default(), &repo.name_with_owner)
            .ok_or_else(|| {
                PrepareError::new(format!(
                    "{} has no git remote to fetch #{number} from; add one with `git remote add origin <url>`",
                    repo.path.display()
                ))
            })?;
        fetch(&mut git, &wt, &remote, number, head).map_err(|failed| match failed {
            Fetch::Failed(failed) => failed.explain(format!(
                "could not fetch #{number} from {remote}: {}; check your network and that `git fetch {remote}` works in {}, then run it again",
                failed.detail,
                repo.path.display()
            )),
            Fetch::Missing => PrepareError::new(format!(
                "#{number}'s head {} is not on {remote}; the pull request may have moved: refresh and run it again",
                short_sha(head)
            )),
        })?;

        let checked_out = git.stream(&wt, &["checkout", "--detach", "--force", head]).and_then(|()| {
            // Untracked files a previous run left behind; ignored ones, such
            // as build caches, stay.
            git.stream(&wt, &["clean", "-fd"])
        });
        if let Err(failed) = checked_out {
            if failed.code != CANCELLED {
                // Start from a fresh worktree next time.
                let _ = self.worktrees.remove_unclaimed(runner, &wt);
            }
            return Err(failed.explain(format!(
                "could not check out {} in Signoff's worktree: {}; run it again to start from a fresh worktree",
                short_sha(head),
                failed.detail
            )));
        }
        match git.quiet(&wt, &["rev-parse", "HEAD"]) {
            Some(actual) if actual == head || (head.len() < actual.len() && actual.starts_with(head)) => Ok(wt),
            actual => Err(PrepareError::new(format!(
                "Signoff's worktree is at {}, not {}; run it again",
                actual.as_deref().map_or("an unknown commit", short_sha),
                short_sha(head)
            ))),
        }
    }
}

/// Why a worktree could not be prepared: the exit code to report, and a
/// message that says how to fix it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrepareError {
    /// [`CANCELLED`] when stopped, otherwise non-zero.
    pub code: i32,
    /// For the person.
    pub message: String,
}

impl PrepareError {
    fn new(message: impl Into<String>) -> Self {
        Self { code: 1, message: message.into() }
    }
}

impl Worktrees {
    /// Worktrees kept in `root`, which should be one of the app's own
    /// folders. It is created when first needed.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), in_use: Arc::default() }
    }

    /// The folder they are kept in.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the worktree for the repository `repo_id` goes, or `None` for
    /// an id that is not safe as a folder name.
    pub fn path_for(&self, repo_id: &str) -> Option<PathBuf> {
        valid_id(repo_id).then(|| self.root.join(repo_id))
    }

    /// Whether `path` is where a managed worktree goes: directly inside the
    /// root (compared on real paths) and named like a repository id. The
    /// only paths this ever deletes.
    pub fn is_managed(&self, path: &Path) -> bool {
        let Some(name) = path.file_name().and_then(OsStr::to_str) else { return false };
        if !valid_id(name) {
            return false;
        }
        let (Some(parent), Ok(root)) = (path.parent(), fs::canonicalize(&self.root)) else { return false };
        // Never a filesystem root, whatever the app was given.
        root.parent().is_some() && fs::canonicalize(parent).is_ok_and(|parent| parent == root)
    }

    /// Takes `repo_id`'s worktree for one run, or `None` while another run
    /// or a cleanup holds it.
    pub fn claim(&self, repo_id: &str) -> Option<Lease> {
        let mut in_use = self.in_use.lock().unwrap_or_else(PoisonError::into_inner);
        in_use.insert(repo_id.to_owned()).then(|| Lease { worktrees: self.clone(), repo_id: repo_id.to_owned() })
    }

    /// Removes the managed worktree at `path`: unregisters it from its
    /// repository (`git worktree remove --force`, then `prune`), and deletes
    /// the folder if that left it behind, as when the repository is gone.
    ///
    /// # Errors
    ///
    /// When `path` is not a managed worktree, is held by a run, or could
    /// not be deleted. Nothing outside the root is ever touched.
    pub fn remove(&self, runner: &dyn Runner, path: &Path) -> Result<(), String> {
        let id = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
        let _lease = self.claim(id).ok_or_else(|| format!("{} is in use by a run", path.display()))?;
        self.remove_unclaimed(runner, path)
    }

    fn remove_unclaimed(&self, runner: &dyn Runner, path: &Path) -> Result<(), String> {
        if !self.is_managed(path) {
            return Err(format!("refusing to remove {}: it is not one of Signoff's worktrees", path.display()));
        }
        match fs::symlink_metadata(path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(format!("could not read {}: {err}", path.display())),
            Ok(meta) if !meta.is_dir() => {
                return Err(format!("refusing to remove {}: it is not a folder", path.display()));
            }
            Ok(_) => {}
        }
        let common = owner(path);
        if let Some(common) = &common {
            let git_dir = common.to_string_lossy();
            let target = path.to_string_lossy();
            let args = ["--git-dir", &git_dir, "worktree", "remove", "--force", &target];
            match runner.run(None, None, "git", &args) {
                Ok(out) if out.code == 0 => {}
                Ok(out) => log::info!("git worktree remove {}: {}", path.display(), out.stderr),
                Err(err) => log::info!("git worktree remove {}: {err}", path.display()),
            }
        }
        if fs::symlink_metadata(path).is_ok() {
            // Checked again: the folder is about to go, with everything in it.
            if !self.is_managed(path) {
                return Err(format!("refusing to remove {}: it is not one of Signoff's worktrees", path.display()));
            }
            fs::remove_dir_all(path).map_err(|err| format!("could not delete {}: {err}", path.display()))?;
        }
        if let Some(common) = &common {
            let git_dir = common.to_string_lossy();
            let _ = runner.run(None, None, "git", &["--git-dir", &git_dir, "worktree", "prune"]);
        }
        Ok(())
    }

    /// Removes every worktree `config` no longer wants: all of them when
    /// the setting is off, otherwise those of repositories that are no
    /// longer tracked or whose checkout is gone. Worktrees a run holds are
    /// left for a later call, and anything in the root that is not a
    /// worktree folder is left alone. Returns what was removed.
    pub fn retain(&self, runner: &dyn Runner, config: &AppConfig) -> Vec<PathBuf> {
        let keep: HashSet<&str> = if config.isolated_worktrees {
            config.repositories.iter().filter(|repo| repo.path.is_dir()).map(|repo| repo.id.as_str()).collect()
        } else {
            HashSet::new()
        };
        let Ok(entries) = fs::read_dir(&self.root) else { return Vec::new() };
        let mut removed = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(id) = name.to_str().filter(|id| valid_id(id) && !keep.contains(id)) else { continue };
            let path = self.root.join(id);
            if !fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                continue;
            }
            match self.remove(runner, &path) {
                Ok(()) => removed.push(path),
                Err(err) => log::warn!("could not remove a worktree: {err}"),
            }
        }
        removed
    }
}

/// Runs `git`, echoing each command as `$ git …` and remembering the last
/// line of output for an error.
struct Git<'a> {
    runner: &'a dyn Runner,
    cancel: &'a AtomicBool,
    on_line: &'a mut dyn FnMut(&str),
}

/// A `git` step that failed: its exit code and last line of output.
struct Failed {
    code: i32,
    detail: String,
}

impl Failed {
    /// The error for the person: `message`, unless the step was stopped
    /// or timed out, which say so instead.
    fn explain(&self, message: String) -> PrepareError {
        match self.code {
            CANCELLED => PrepareError { code: CANCELLED, message: "stopped".to_owned() },
            TIMED_OUT => PrepareError {
                code: TIMED_OUT,
                message: format!("git took longer than {}; {message}", human_duration(GIT_TIMEOUT)),
            },
            _ => PrepareError::new(message),
        }
    }
}

impl Git<'_> {
    fn stream(&mut self, dir: &Path, args: &[&str]) -> Result<(), Failed> {
        (self.on_line)(&format!("$ git {}", args.join(" ")));
        let mut last = String::new();
        let on_line = &mut *self.on_line;
        let code = self
            .runner
            .stream(Some(dir), "git", args, GIT_TIMEOUT, self.cancel, &mut |line| {
                if !line.trim().is_empty() {
                    line.trim().clone_into(&mut last);
                }
                on_line(line);
            })
            .map_err(|err| Failed { code: 1, detail: format!("could not start git: {err}") })?;
        if code == 0 {
            Ok(())
        } else {
            let detail = if last.is_empty() { format!("git exited with {code}") } else { last };
            Err(Failed { code, detail })
        }
    }

    /// A quick query: its trimmed stdout, or `None` when it failed.
    fn quiet(&self, dir: &Path, args: &[&str]) -> Option<String> {
        let out = self.runner.run(Some(dir), None, "git", args).ok()?;
        (out.code == 0).then_some(out.stdout)
    }
}

/// Why fetching the head did not give the commit.
enum Fetch {
    /// No fetch worked: the network, sign-in or remote.
    Failed(Failed),
    /// The remote answered but does not have the commit: it moved.
    Missing,
}

/// Fetches pull request `number`'s head into the worktree until `head` is
/// there: `refs/pull/<n>/head` first, which GitHub keeps on the base
/// repository for forks too, then the commit itself. Only `FETCH_HEAD`,
/// which is the worktree's own, is written; no branch moves.
fn fetch(git: &mut Git<'_>, wt: &Path, remote: &str, number: u64, head: &str) -> Result<(), Fetch> {
    let present = |git: &Git<'_>| git.quiet(wt, &["cat-file", "-e", &format!("{head}^{{commit}}")]).is_some();
    let pull = format!("refs/pull/{number}/head");
    let first = git.stream(wt, &["fetch", "--no-tags", remote, &pull]);
    if let Err(failed) = &first
        && failed.code == CANCELLED
    {
        return Err(Fetch::Failed(Failed { code: CANCELLED, detail: String::new() }));
    }
    if present(git) {
        return Ok(());
    }
    let second = git.stream(wt, &["fetch", "--no-tags", remote, head]);
    if present(git) {
        return Ok(());
    }
    match (first, second) {
        (Err(failed), Err(_)) | (Ok(()), Err(failed @ Failed { code: CANCELLED, .. })) => Err(Fetch::Failed(failed)),
        _ => Err(Fetch::Missing),
    }
}

/// Whether `wt` is a worktree of the repository whose shared git directory
/// is `common`, with its git directory still there.
fn reusable(wt: &Path, common: &Path) -> bool {
    fs::symlink_metadata(wt).is_ok_and(|meta| meta.is_dir())
        && owner(wt).and_then(|owner| fs::canonicalize(owner).ok()).is_some_and(|owner| owner == common)
}

/// The shared git directory of the repository `wt` is a worktree of, read
/// from its `.git` file (never from a folder above it). `None` when it is
/// not a linked worktree or its repository is gone.
fn owner(wt: &Path) -> Option<PathBuf> {
    let dot_git = wt.join(".git");
    if !fs::symlink_metadata(&dot_git).is_ok_and(|meta| meta.is_file()) {
        return None;
    }
    let text = fs::read_to_string(&dot_git).ok()?;
    let git_dir = clean(&wt.join(text.trim().strip_prefix("gitdir:")?.trim()));
    if !git_dir.join("HEAD").is_file() {
        return None;
    }
    let common = clean(&git::common_dir(&git_dir));
    common.is_dir().then_some(common)
}

/// Lexically removes `.` and `..`, so the path handed to `git` stays a
/// plain one (canonical paths on Windows are verbatim, which git rejects).
fn clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// The remote to fetch from: the one whose URL names `name_with_owner`
/// (`git@github.com:o/r.git`, `https://github.com/o/r`), else `origin`,
/// else the first. `remotes` is `git remote -v` output.
pub(crate) fn pick_remote(remotes: &str, name_with_owner: &str) -> Option<String> {
    let fetch: Vec<(&str, &str)> = remotes
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let (name, url, kind) = (parts.next()?, parts.next()?, parts.next().unwrap_or("(fetch)"));
            (kind == "(fetch)" && !name.starts_with('-')).then_some((name, url))
        })
        .collect();
    let wanted = name_with_owner.trim().to_lowercase();
    let names = |url: &str| {
        let url = url.trim_end_matches('/').to_lowercase();
        let url = url.strip_suffix(".git").unwrap_or(&url);
        !wanted.is_empty() && (url.ends_with(&format!("/{wanted}")) || url.ends_with(&format!(":{wanted}")))
    };
    fetch
        .iter()
        .find(|(_, url)| names(url))
        .or_else(|| fetch.iter().find(|(name, _)| *name == "origin"))
        .or_else(|| fetch.first())
        .map(|(name, _)| (*name).to_owned())
}

/// A repository id as [`crate::config::repository_id`] makes them: short
/// and plain enough to be a folder name anywhere.
fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A commit id that cannot be read as an option or a ref expression.
fn valid_commit(sha: &str) -> bool {
    (7..=64).contains(&sha.len()) && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::runner::SystemRunner;
    use std::process::Command;

    /// Runs `git` in `dir` and returns its trimmed stdout.
    pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=T", "-c", "user.email=t@example.test", "-c", "init.defaultBranch=main"])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// A GitHub-like origin with pull request refs, and the person's clone.
    pub(crate) struct Repos {
        pub(crate) _dir: tempfile::TempDir,
        pub(crate) checkout: PathBuf,
        pub(crate) root: PathBuf,
        /// `main`'s commit, which the checkout has checked out.
        pub(crate) main: String,
        /// The head of pull request 7.
        pub(crate) pr7: String,
        /// The head of pull request 8, on top of 7.
        pub(crate) pr8: String,
    }

    impl Repos {
        pub(crate) fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let origin = dir.path().join("origin");
            fs::create_dir_all(&origin).expect("mkdir");
            git(&origin, &["init", "-q"]);
            fs::write(origin.join("README"), "one\n").expect("write");
            git(&origin, &["add", "README"]);
            git(&origin, &["commit", "-qm", "one"]);
            let main = git(&origin, &["rev-parse", "HEAD"]);
            git(&origin, &["checkout", "-qb", "topic"]);
            fs::write(origin.join("feature.txt"), "seven\n").expect("write");
            git(&origin, &["add", "feature.txt"]);
            git(&origin, &["commit", "-qm", "seven"]);
            let pr7 = git(&origin, &["rev-parse", "HEAD"]);
            fs::write(origin.join("feature.txt"), "eight\n").expect("write");
            git(&origin, &["commit", "-qam", "eight"]);
            let pr8 = git(&origin, &["rev-parse", "HEAD"]);
            git(&origin, &["checkout", "-q", "main"]);
            // As GitHub keeps them: not branches, so a clone does not get them.
            git(&origin, &["update-ref", "refs/pull/7/head", &pr7]);
            git(&origin, &["update-ref", "refs/pull/8/head", &pr8]);
            git(&origin, &["branch", "-qD", "topic"]);
            // Not a local clone, which would copy every object: only what a
            // real clone has.
            git(dir.path(), &["clone", "-q", "--no-local", "origin", "checkout"]);
            let checkout = dir.path().join("checkout");
            let root = dir.path().join("cache").join("worktrees");
            Self { checkout, root, main, pr7, pr8, _dir: dir }
        }

        pub(crate) fn repo(&self) -> TrackedRepository {
            TrackedRepository {
                id: "a1b2c3d4e5f60718".into(),
                path: self.checkout.clone(),
                name_with_owner: "o/app".into(),
                enabled: true,
                ..TrackedRepository::default()
            }
        }

        fn worktrees(&self) -> Worktrees {
            Worktrees::new(&self.root)
        }

        /// Everything that says what the person's checkout has checked out.
        pub(crate) fn checkout_state(&self) -> (String, String, String, String) {
            (
                git(&self.checkout, &["rev-parse", "--abbrev-ref", "HEAD"]),
                git(&self.checkout, &["rev-parse", "HEAD"]),
                git(&self.checkout, &["status", "--porcelain"]),
                git(&self.checkout, &["for-each-ref", "refs/heads", "refs/remotes"]),
            )
        }

        fn worktree_count(&self) -> usize {
            git(&self.checkout, &["worktree", "list", "--porcelain"])
                .lines()
                .filter(|line| line.starts_with("worktree "))
                .count()
        }
    }

    fn prepare(
        worktrees: &Worktrees,
        repo: &TrackedRepository,
        number: u64,
        head: &str,
    ) -> (Result<PathBuf, PrepareError>, Vec<String>) {
        let lease = worktrees.claim(&repo.id).expect("free");
        let mut lines = Vec::new();
        let result =
            lease.prepare(&SystemRunner::default(), repo, number, head, &AtomicBool::new(false), &mut |line| {
                lines.push(line.to_owned())
            });
        (result, lines)
    }

    #[test]
    fn prepares_the_head_in_its_own_folder_and_leaves_the_checkout_alone() {
        let repos = Repos::new();
        let before = repos.checkout_state();
        assert_eq!(before.1, repos.main);
        let watch = git::watch_paths(&repos.checkout).expect("a checkout");
        let (result, lines) = prepare(&repos.worktrees(), &repos.repo(), 7, &repos.pr7);
        let wt = result.expect("prepared");

        assert_eq!(wt, repos.root.join("a1b2c3d4e5f60718"));
        assert!(!fs::canonicalize(&wt).expect("real").starts_with(fs::canonicalize(&repos.checkout).expect("real")));
        assert_eq!(git(&wt, &["rev-parse", "HEAD"]), repos.pr7);
        assert_eq!(git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]), "HEAD", "detached");
        assert_eq!(fs::read_to_string(wt.join("feature.txt")).expect("checked out"), "seven\n");
        assert!(lines.iter().any(|l| l.starts_with("$ git fetch --no-tags origin refs/pull/7/head")), "{lines:?}");

        assert_eq!(repos.checkout_state(), before, "no branch, HEAD or file of the checkout moved");
        // The only HEAD that moved is the worktree's own, which the
        // checkout's watcher does not count.
        let own = fs::canonicalize(git::find_git_dir(&wt).expect("git dir")).expect("real");
        assert!(own.join("HEAD").is_file());
        for name in ["HEAD", "index", "FETCH_HEAD", "ORIG_HEAD"] {
            assert!(!watch.affects_head(&own.join(name)), "{name}");
        }
    }

    #[test]
    fn reuses_the_worktree_for_the_next_pull_request_and_drops_stray_files() {
        let repos = Repos::new();
        let worktrees = repos.worktrees();
        let wt = prepare(&worktrees, &repos.repo(), 7, &repos.pr7).0.expect("first");
        fs::write(wt.join("feature.txt"), "edited by a formatter\n").expect("write");
        fs::write(wt.join("stray.txt"), "left behind\n").expect("write");

        let again = prepare(&worktrees, &repos.repo(), 8, &repos.pr8).0.expect("second");
        assert_eq!(again, wt);
        assert_eq!(git(&wt, &["rev-parse", "HEAD"]), repos.pr8);
        assert_eq!(fs::read_to_string(wt.join("feature.txt")).expect("read"), "eight\n");
        assert!(!wt.join("stray.txt").exists());
        assert_eq!(repos.worktree_count(), 2, "the checkout and one managed worktree");
    }

    #[test]
    fn a_deleted_worktree_folder_is_made_again() {
        let repos = Repos::new();
        let worktrees = repos.worktrees();
        let wt = prepare(&worktrees, &repos.repo(), 7, &repos.pr7).0.expect("first");
        fs::remove_dir_all(&wt).expect("delete");
        prepare(&worktrees, &repos.repo(), 7, &repos.pr7).0.expect("made again");
        assert_eq!(git(&wt, &["rev-parse", "HEAD"]), repos.pr7);
        assert_eq!(repos.worktree_count(), 2);
    }

    #[test]
    fn a_failed_fetch_says_how_to_fix_it() {
        let repos = Repos::new();
        let unknown = "0123456789abcdef0123456789abcdef01234567";
        let (result, _) = prepare(&repos.worktrees(), &repos.repo(), 9, unknown);
        let err = result.expect_err("nothing to fetch");
        assert!(err.message.contains("could not fetch #9 from origin"), "{}", err.message);
        assert!(err.message.contains("`git fetch origin` works in"), "{}", err.message);

        let (result, _) = prepare(&repos.worktrees(), &repos.repo(), 7, unknown);
        let err = result.expect_err("moved");
        assert!(err.message.contains("may have moved"), "{}", err.message);
    }

    #[test]
    fn refuses_a_root_inside_the_checkout() {
        let repos = Repos::new();
        let worktrees = Worktrees::new(repos.checkout.join("worktrees"));
        let err = prepare(&worktrees, &repos.repo(), 7, &repos.pr7).0.expect_err("overlap");
        assert!(err.message.contains("overlaps the checkout"), "{}", err.message);
        assert_eq!(repos.worktree_count(), 1);
    }

    #[test]
    fn one_run_holds_a_worktree_at_a_time() {
        let worktrees = Worktrees::new("/nowhere");
        let lease = worktrees.claim("r1").expect("free");
        assert!(worktrees.claim("r1").is_none());
        assert!(worktrees.claim("r2").is_some());
        drop(lease);
        assert!(worktrees.claim("r1").is_some());
    }

    #[test]
    fn cleanup_follows_the_setting_and_the_tracked_repositories() {
        let repos = Repos::new();
        let worktrees = repos.worktrees();
        let runner = SystemRunner::default();
        let wt = prepare(&worktrees, &repos.repo(), 7, &repos.pr7).0.expect("prepared");
        let on = AppConfig { isolated_worktrees: true, repositories: vec![repos.repo()], ..AppConfig::default() };

        assert!(worktrees.retain(&runner, &on).is_empty(), "tracked and on: kept");
        // A run holds it: kept even with the setting off.
        let lease = worktrees.claim(&repos.repo().id).expect("free");
        assert!(worktrees.retain(&runner, &AppConfig::default()).is_empty());
        drop(lease);

        let untracked = AppConfig { isolated_worktrees: true, ..AppConfig::default() };
        assert_eq!(worktrees.retain(&runner, &untracked), std::slice::from_ref(&wt));
        assert!(!wt.exists());
        assert_eq!(repos.worktree_count(), 1, "unregistered from the checkout too");

        prepare(&worktrees, &repos.repo(), 7, &repos.pr7).0.expect("prepared");
        assert_eq!(worktrees.retain(&runner, &AppConfig::default()), std::slice::from_ref(&wt), "the setting is off");
        assert_eq!(repos.worktree_count(), 1);
    }

    #[test]
    fn a_worktree_whose_repository_is_gone_is_deleted() {
        let repos = Repos::new();
        let worktrees = repos.worktrees();
        let wt = prepare(&worktrees, &repos.repo(), 7, &repos.pr7).0.expect("prepared");
        fs::remove_dir_all(&repos.checkout).expect("delete the checkout");
        let config = AppConfig { isolated_worktrees: true, repositories: vec![repos.repo()], ..AppConfig::default() };
        assert_eq!(worktrees.retain(&SystemRunner::default(), &config), std::slice::from_ref(&wt));
        assert!(!wt.exists());
    }

    #[test]
    fn nothing_outside_the_root_or_unlike_a_worktree_is_deleted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("worktrees");
        let outside = dir.path().join("precious");
        fs::create_dir_all(&outside).expect("mkdir");
        fs::write(outside.join("keep.txt"), "mine").expect("write");
        fs::create_dir_all(root.join("My Folder")).expect("mkdir");
        fs::write(root.join("notes.txt"), "mine").expect("write");
        let worktrees = Worktrees::new(&root);
        let runner = SystemRunner::default();

        assert!(!worktrees.is_managed(&root), "the root itself");
        assert!(!worktrees.is_managed(dir.path()));
        assert!(!worktrees.is_managed(&outside));
        assert!(!worktrees.is_managed(&root.join("..").join("precious")));
        assert!(!worktrees.is_managed(&root.join("My Folder")));
        assert!(!worktrees.is_managed(&root.join("abc").join("def")), "only direct children");
        assert!(worktrees.is_managed(&root.join("abc")));
        let top = Worktrees::new(dir.path().ancestors().last().expect("a filesystem root"));
        assert!(top.path_for("abc").is_some_and(|path| !top.is_managed(&path)), "never a filesystem root");

        assert!(worktrees.remove(&runner, &outside).is_err());
        assert!(worktrees.remove(&runner, &root.join("..").join("precious")).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, root.join("linked")).expect("symlink");
            assert!(worktrees.remove(&runner, &root.join("linked")).is_err(), "a link is not a folder");
        }
        assert!(worktrees.retain(&runner, &AppConfig::default()).is_empty());
        assert!(outside.join("keep.txt").is_file());
        assert!(root.join("notes.txt").is_file() && root.join("My Folder").is_dir());
    }

    #[test]
    fn picks_the_remote_that_names_the_repository() {
        let remotes = "fork\tgit@github.com:me/app.git (fetch)\nfork\tgit@github.com:me/app.git (push)\n\
                       upstream\thttps://github.com/O/App.git (fetch)\nupstream\thttps://github.com/O/App.git (push)\n";
        assert_eq!(pick_remote(remotes, "o/app").as_deref(), Some("upstream"));
        assert_eq!(pick_remote("x\t/srv/app (fetch)\norigin\t/srv/app (fetch)\n", "o/app").as_deref(), Some("origin"));
        assert_eq!(pick_remote("x\t/srv/app (fetch)\n", "o/app").as_deref(), Some("x"));
        assert_eq!(pick_remote("origin\tgit@github.com:o/app (fetch)\n", "o/app").as_deref(), Some("origin"));
        assert_eq!(pick_remote("", "o/app"), None);
    }

    #[test]
    fn ids_and_commits_are_checked_before_reaching_git() {
        assert!(valid_id("a1b2c3d4e5f60718") && valid_id("demo-rails"));
        assert!(!valid_id("") && !valid_id("..") && !valid_id("a/b") && !valid_id("a b"));
        assert!(valid_commit("9f3a201") && !valid_commit("--upload-pack=x") && !valid_commit("HEAD"));
    }
}
