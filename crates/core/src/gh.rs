//! Reads from GitHub through the `gh` CLI or, when [`Gh::with_api`] gave it
//! one, the in-process [`Api`] for repository reads; and from the local
//! checkout through the files under `.git`, or `git`.

use std::fmt;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

use crate::api::{Api, ApiError, Transport};
use crate::checks::{CheckState, is_signoff_context};
use crate::runner::{Output, Runner, SystemRunner, TIMED_OUT};

/// Open pull requests read per repository.
pub const PULL_REQUEST_LIMIT: usize = 30;

/// `gh` processes reading repositories at once, across all of them. Each
/// one is a GraphQL or REST call of its own; started all together for
/// every repository they queued behind each other inside `gh` and the
/// network, and every one hit its timeout.
pub const PARALLEL_GH_READS: usize = 2;
/// How long a `gh` read may take at first. Listing pull requests with
/// their status rollup is a heavy GraphQL query, so this is longer than
/// the runner's default.
pub const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// The longest a `gh` read may take. Each read that times out doubles the
/// limit up to this, and one that finishes sets it back, so a slow network
/// gets reads through instead of failing every one at the same mark.
pub const MAX_READ_TIMEOUT: Duration = Duration::from_secs(180);

const PULL_REQUEST_FIELDS: &str =
    "number,title,url,state,author,headRefOid,headRefName,baseRefName,mergeable,isDraft,updatedAt";

/// `statusCheckRollup` rides along on the pull-request list, which keeps a
/// refresh to a couple of calls per repository instead of one per pull
/// request. It carries context, state and time but not the creator or
/// description, so attribution still needs [`Gh::commit_statuses`]; the
/// product's status cache only pays for that when something changed.
const PULL_REQUEST_LIST_FIELDS: &str =
    "number,title,url,state,author,headRefOid,headRefName,baseRefName,mergeable,isDraft,updatedAt,statusCheckRollup";

/// Why a `gh` or `git` call failed.
#[derive(Debug, thiserror::Error)]
pub enum GhError {
    /// The program could not be started (most often: `gh` is not installed).
    #[error("launching {program}: {source}")]
    Launch {
        /// The program that failed to start.
        program: String,
        /// Why.
        source: std::io::Error,
    },
    /// The program ran and exited non-zero.
    #[error("{command}: exit {code}: {message}")]
    Exit {
        /// The command line, for the message.
        command: String,
        /// Its exit code.
        code: i32,
        /// Its stderr, or stdout when stderr was empty.
        message: String,
    },
    /// The program's output was not the JSON expected.
    #[error("parsing output of {command}: {source}")]
    Json {
        /// The command line, for the message.
        command: String,
        /// Why.
        source: serde_json::Error,
    },
    /// A read through the REST API failed.
    #[error("GitHub API: {0}")]
    Api(#[from] ApiError),
}

impl GhError {
    /// The message of a non-zero exit or an API error answer, if that is
    /// what this is.
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Exit { message, .. } | Self::Api(ApiError::Status { message, .. }) => Some(message),
            _ => None,
        }
    }

    /// Whether GitHub answered 404/403. Branch protection does that for a
    /// caller without admin, the common case for contributors, which must not
    /// be fatal.
    pub fn is_not_found(&self) -> bool {
        self.message().is_some_and(|m| {
            ["404", "Not Found", "403", "Branch not protected", "Resource not accessible"]
                .iter()
                .any(|needle| m.contains(needle))
        })
    }

    /// Whether this says `gh` is missing or signed out, rather than that one
    /// repository could not be read. The environment is probed again after
    /// one of these, since the cached probe no longer holds.
    pub fn is_environment_failure(&self) -> bool {
        match self {
            Self::Launch { .. } => true,
            Self::Exit { message, .. } => {
                let message = message.to_ascii_lowercase();
                [
                    "gh auth login",
                    "not logged",
                    "authentication",
                    "bad credentials",
                    "http 401",
                    "requires authentication",
                ]
                .iter()
                .any(|needle| message.contains(needle))
            }
            Self::Api(err) => matches!(err, ApiError::Unauthorized),
            Self::Json { .. } => false,
        }
    }
}

/// Everything that can be wrong before there is anything to show. The UI
/// renders a specific remedy for each missing piece.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Environment {
    /// `gh --version` ran.
    pub gh_installed: bool,
    /// What it reported, without the `gh version ` prefix.
    pub gh_version: String,
    /// `gh api user` answered with a login.
    pub authenticated: bool,
    /// That login.
    pub login: String,
    /// The probed directory is inside a git checkout.
    pub in_git_repo: bool,
    /// The `gh signoff` extension is installed.
    pub signoff_installed: bool,
    /// Its version.
    pub signoff_version: String,
}

/// A local checkout and the GitHub repository behind it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RepoInfo {
    /// `owner/name`.
    pub name_with_owner: String,
    /// The GitHub host: `github.com`, or a GitHub Enterprise one.
    pub host: String,
    /// The default branch on GitHub.
    pub default_branch: String,
    /// The branch checked out locally.
    pub current_branch: String,
    /// The commit checked out locally.
    pub head_sha: String,
}

/// One open pull request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PrInfo {
    /// Its number.
    pub number: u64,
    /// Its title.
    pub title: String,
    /// The author's login.
    pub author: String,
    /// Its web URL.
    pub url: String,
    /// `OPEN`, `CLOSED` or `MERGED`.
    pub state: String,
    /// The head commit.
    pub head_sha: String,
    /// The head branch.
    pub head_ref: String,
    /// The branch it merges into, whose protection gates it.
    pub base_ref: String,
    /// `MERGEABLE`, `CONFLICTING` or `UNKNOWN`.
    pub mergeable: String,
    /// Whether it is a draft.
    pub is_draft: bool,
    /// When it last changed.
    pub updated_at: Timestamp,
    /// The signoff-relevant commit statuses from `statusCheckRollup`. Empty
    /// when the pull request came from [`Gh::current_pull_request`].
    pub contexts: Vec<CommitStatus>,
}

/// One commit status.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CommitStatus {
    /// `signoff`, `signoff/tests`, `ci/build`…
    pub context: String,
    /// Its state, in the REST API's lower-case spelling.
    pub state: CheckState,
    /// Free text; gh-signoff writes `<user> signed off`.
    pub description: String,
    /// Who posted it. GitHub sometimes reports none.
    pub creator: String,
    /// The creator's avatar.
    pub avatar_url: String,
    /// When it was posted.
    pub created_at: Timestamp,
    /// Where it links.
    pub target_url: String,
}

/// The three outcomes of reading branch protection, which GitHub reports
/// with the same 404:
///
/// - `{"message":"Branch not protected"}`: the branch has no rules.
/// - `{"message":"Not Found"}`: the caller lacks admin, or it is gone.
///
/// Collapsing them would either tell an owner their permissions are wrong,
/// or tell a contributor that nothing is required when something is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtectionStatus {
    /// Read, and these are the rules.
    Ok,
    /// The branch has no protection.
    Absent,
    /// Not readable with the caller's permissions.
    Unreadable,
}

/// `program`'s stdout, or why it could not be started or exited non-zero.
fn finish(program: &str, args: &[&str], out: std::io::Result<Output>) -> Result<String, GhError> {
    let out = out.map_err(|source| GhError::Launch { program: program.to_owned(), source })?;
    if out.code != 0 {
        let message = if out.stderr.is_empty() { out.stdout } else { out.stderr };
        return Err(GhError::Exit { command: format!("{program} {}", args.join(" ")), code: out.code, message });
    }
    Ok(out.stdout)
}

/// `stdout` of `gh args` parsed as JSON.
fn parse_json<T: DeserializeOwned>(args: &[&str], stdout: &str) -> Result<T, GhError> {
    serde_json::from_str(stdout).map_err(|source| GhError::Json { command: format!("gh {}", args.join(" ")), source })
}

/// A `gh` (and `git`) client.
#[derive(Clone)]
pub struct Gh {
    runner: Arc<dyn Runner>,
    api: Option<Arc<Api>>,
    reads: Arc<Reads>,
}

/// Limits `gh` reads: how many run at once, and how long each may take.
#[derive(Debug)]
struct Reads {
    running: Mutex<usize>,
    freed: Condvar,
    timeout: Mutex<Duration>,
}

impl Default for Reads {
    fn default() -> Self {
        Self { running: Mutex::new(0), freed: Condvar::new(), timeout: Mutex::new(READ_TIMEOUT) }
    }
}

impl Reads {
    /// Waits for a free slot; the read holds it until the guard drops.
    fn enter(&self) -> ReadSlot<'_> {
        let mut running = lock(&self.running);
        while *running >= PARALLEL_GH_READS {
            running = self.freed.wait(running).unwrap_or_else(PoisonError::into_inner);
        }
        *running += 1;
        ReadSlot(self)
    }

    fn timeout(&self) -> Duration {
        *lock(&self.timeout)
    }

    /// Lengthens the limit after a read timed out, resets it after one
    /// that finished.
    fn note(&self, timed_out: bool) {
        let mut timeout = lock(&self.timeout);
        *timeout = if timed_out { timeout.saturating_mul(2).min(MAX_READ_TIMEOUT) } else { READ_TIMEOUT };
    }
}

struct ReadSlot<'a>(&'a Reads);

impl Drop for ReadSlot<'_> {
    fn drop(&mut self) {
        *lock(&self.0.running) -= 1;
        self.0.freed.notify_one();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl fmt::Debug for Gh {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Gh").finish_non_exhaustive()
    }
}

impl Default for Gh {
    fn default() -> Self {
        Self::new(Arc::new(SystemRunner::default()))
    }
}

impl Gh {
    /// A client that spawns processes through `runner` for everything.
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self { runner, api: None, reads: Arc::default() }
    }

    /// A client that reads repositories over the REST API through
    /// `transport` when `gh auth token` gives a token, and spawns processes
    /// through `runner` for everything else.
    pub fn with_api(runner: Arc<dyn Runner>, transport: Arc<dyn Transport>) -> Self {
        let api = Arc::new(Api::new(transport, runner.clone()));
        Self { runner, api: Some(api), reads: Arc::default() }
    }

    /// The REST client, if this has one.
    pub fn api(&self) -> Option<&Arc<Api>> {
        self.api.as_ref()
    }

    /// When reading may start again, if GitHub's rate limit stopped it.
    pub fn next_allowed(&self) -> Option<Timestamp> {
        self.api.as_ref().and_then(|api| api.next_allowed())
    }

    fn exec(&self, dir: Option<&Path>, program: &str, args: &[&str]) -> Result<String, GhError> {
        self.run(dir, None, program, args)
    }

    /// Runs `program` with `input` on stdin; its stdout, or an error when it
    /// could not start or exited non-zero.
    pub(crate) fn run(
        &self,
        dir: Option<&Path>,
        input: Option<&str>,
        program: &str,
        args: &[&str],
    ) -> Result<String, GhError> {
        let out = self.runner.run(dir, input, program, args);
        finish(program, args, out)
    }

    /// Runs `gh` to read a repository: a few at a time, under the read
    /// timeout, which grows while reads keep timing out.
    fn read(&self, dir: &Path, args: &[&str]) -> Result<String, GhError> {
        let out = {
            let _slot = self.reads.enter();
            self.runner.run_for(Some(dir), None, "gh", args, self.reads.timeout())
        };
        if let Ok(out) = &out {
            self.reads.note(out.code == TIMED_OUT);
        }
        finish("gh", args, out)
    }

    /// [`Gh::read`], parsed as JSON.
    fn read_json<T: DeserializeOwned>(&self, dir: &Path, args: &[&str]) -> Result<T, GhError> {
        parse_json(args, &self.read(dir, args)?)
    }

    fn json<T: DeserializeOwned>(&self, dir: Option<&Path>, args: &[&str]) -> Result<T, GhError> {
        parse_json(args, &self.exec(dir, "gh", args)?)
    }

    fn git(&self, dir: &Path, args: &[&str]) -> Option<String> {
        self.exec(Some(dir), "git", args).ok()
    }

    /// Checks `gh`, its sign-in, the checkout at `dir` and the extension.
    pub fn probe_environment(&self, dir: Option<&Path>) -> Environment {
        let mut env = Environment::default();

        if let Ok(out) = self.exec(None, "gh", &["--version"]) {
            env.gh_installed = true;
            let line = out.lines().next().unwrap_or_default();
            env.gh_version = line.trim_start_matches("gh version ").to_owned();
        }

        #[derive(Deserialize)]
        struct Account {
            #[serde(default)]
            login: String,
        }
        if let Ok(account) = self.json::<Account>(None, &["api", "user", "--jq", "{login: .login}"]) {
            env.authenticated = !account.login.is_empty();
            env.login = account.login;
        }

        if self.exec(dir, "git", &["rev-parse", "--git-dir"]).is_ok() {
            env.in_git_repo = true;
        }

        // `gh signoff version` prints "gh-signoff 0.3.0"; a non-zero exit
        // means the extension is not installed.
        if let Ok(out) = self.exec(dir, "gh", &["signoff", "version"]) {
            env.signoff_installed = true;
            env.signoff_version = out.trim_start_matches("gh-signoff").trim().to_owned();
        }
        env
    }

    /// The GitHub repository behind the checkout at `dir`, and what is
    /// checked out there.
    ///
    /// # Errors
    ///
    /// When `gh repo view` fails: not a GitHub checkout, or not signed in.
    pub fn repo(&self, dir: &Path) -> Result<RepoInfo, GhError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct View {
            name_with_owner: String,
            #[serde(default)]
            default_branch_ref: Option<Branch>,
            #[serde(default)]
            url: String,
        }
        #[derive(Deserialize)]
        struct Branch {
            name: String,
        }
        let view: View = self.read_json(dir, &["repo", "view", "--json", "nameWithOwner,defaultBranchRef,url"])?;
        Ok(RepoInfo {
            name_with_owner: view.name_with_owner,
            host: crate::api::host_of(&view.url),
            default_branch: view.default_branch_ref.map(|b| b.name).unwrap_or_default(),
            ..self.local_head(dir)
        })
    }

    /// The branch and commit checked out at `dir`, from the files under
    /// `.git` when they say for certain, otherwise from `git`. Empty when
    /// neither can tell (an unborn branch).
    pub fn local_head(&self, dir: &Path) -> RepoInfo {
        if let Some(head) = crate::git::read_head(dir) {
            return RepoInfo { current_branch: head.branch, head_sha: head.sha, ..RepoInfo::default() };
        }
        RepoInfo {
            current_branch: self.git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default(),
            head_sha: self.git(dir, &["rev-parse", "HEAD"]).unwrap_or_default(),
            ..RepoInfo::default()
        }
    }

    /// The pull request for the branch checked out at `dir`. No pull request
    /// is an ordinary state (`Ok(None)`): you are often on a branch before
    /// opening one.
    ///
    /// # Errors
    ///
    /// When `gh pr view` fails for any other reason.
    pub fn current_pull_request(&self, dir: &Path) -> Result<Option<PrInfo>, GhError> {
        match self.json::<RawPr>(Some(dir), &["pr", "view", "--json", PULL_REQUEST_FIELDS]) {
            Ok(raw) => Ok(Some(raw.into_info())),
            Err(err)
                if err.is_not_found()
                    || err.to_string().contains("no pull requests found")
                    || err.to_string().contains("no open pull requests") =>
            {
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }

    /// Pull request `number` in the repository at `dir`, read fresh, so a
    /// signoff can confirm its head did not move while a command ran.
    ///
    /// # Errors
    ///
    /// When `gh pr view` fails.
    pub fn pull_request(&self, dir: &Path, number: u64) -> Result<PrInfo, GhError> {
        let number = number.to_string();
        self.json::<RawPr>(Some(dir), &["pr", "view", &number, "--json", PULL_REQUEST_FIELDS]).map(RawPr::into_info)
    }

    /// The open pull requests in the repository at `dir`: the user's own, or
    /// everyone's when `all`. The second value says the list was cut at
    /// [`PULL_REQUEST_LIMIT`].
    ///
    /// # Errors
    ///
    /// When `gh pr list` fails.
    pub fn pull_requests(&self, dir: &Path) -> Result<(Vec<PrInfo>, bool), GhError> {
        let limit = (PULL_REQUEST_LIMIT + 1).to_string();
        let args = ["pr", "list", "--state", "open", "--limit", &limit, "--json", PULL_REQUEST_LIST_FIELDS];
        let raw: Vec<RawPr> = self.read_json(dir, &args)?;
        let (raw, truncated) = limit_pull_requests(raw);
        Ok((raw.into_iter().map(RawPr::into_info).collect(), truncated))
    }

    /// Every status posted to `sha`, newest first, with who posted it.
    ///
    /// # Errors
    ///
    /// When the status API call fails.
    pub fn commit_statuses(&self, dir: &Path, repo: &str, sha: &str) -> Result<Vec<CommitStatus>, GhError> {
        let path = format!("repos/{repo}/commits/{sha}/status");
        let stdout = self.read(dir, &["api", &path])?;
        parse_combined_status(&stdout).map_err(|source| GhError::Json { command: format!("gh api {path}"), source })
    }

    /// The status contexts branch protection requires on `branch`, and which
    /// of the three outcomes reading it had.
    pub fn protection_contexts(&self, dir: &Path, repo: &str, branch: &str) -> (Vec<String>, ProtectionStatus) {
        let path = format!("repos/{repo}/branches/{branch}/protection");
        match self.read_json::<Vec<String>>(dir, &["api", &path, "--jq", "[.required_status_checks.contexts[]?]"]) {
            Ok(contexts) => (contexts, ProtectionStatus::Ok),
            Err(err) if err.message().is_some_and(|m| m.contains("Branch not protected")) => {
                (Vec::new(), ProtectionStatus::Absent)
            }
            Err(_) => (Vec::new(), ProtectionStatus::Unreadable),
        }
    }

    /// Refuses unless the checkout at `dir` is at `expected`, so a command
    /// never vouches for a commit it did not run on.
    ///
    /// # Errors
    ///
    /// A message for the user when it is not, or cannot be checked.
    pub fn require_checked_out_commit(&self, dir: &Path, expected: &str) -> Result<(), String> {
        let actual = self.exec(Some(dir), "git", &["rev-parse", "HEAD"]).map_err(|err| {
            let detail = err.message().filter(|m| !m.is_empty()).unwrap_or("git rev-parse failed").to_owned();
            format!("could not verify the checked-out commit: {detail}")
        })?;
        if actual != expected {
            return Err(format!(
                "this checkout is at {}, not pull request commit {}; check out the PR branch and refresh",
                short_sha(&actual),
                short_sha(expected)
            ));
        }
        Ok(())
    }

    /// Checks out pull request `number` in the checkout at `dir` with
    /// `gh pr checkout`, unless tracked files have uncommitted changes
    /// (untracked files are fine: a checkout never overwrites them without
    /// failing).
    ///
    /// # Errors
    ///
    /// A message for the user: the tree is dirty, or `git` or `gh` failed.
    pub fn check_out_pull_request(&self, dir: &Path, number: u64) -> Result<(), String> {
        let detail = |err: &GhError, fallback: &str| {
            err.message().filter(|message| !message.is_empty()).unwrap_or(fallback).to_owned()
        };
        let status = self
            .exec(Some(dir), "git", &["status", "--porcelain", "--untracked-files=no"])
            .map_err(|err| format!("could not read the working tree: {}", detail(&err, "git status failed")))?;
        if !status.trim().is_empty() {
            let changed = status.lines().filter(|line| !line.trim().is_empty()).count();
            let (plural, them) = if changed == 1 { ("", "it") } else { ("s", "them") };
            return Err(format!(
                "this checkout has {changed} uncommitted change{plural}; commit or stash {them} first, then check out #{number}"
            ));
        }
        let arg = number.to_string();
        self.exec(Some(dir), "gh", &["pr", "checkout", &arg])
            .map(drop)
            .map_err(|err| format!("could not check out #{number}: {}", detail(&err, "gh pr checkout failed")))
    }

    /// The commit `branch` is at, preferring the copy fetched from `origin`
    /// over the local branch, which may lag behind.
    pub(crate) fn branch_commit(&self, dir: &Path, branch: &str) -> Option<String> {
        if branch.is_empty() || branch.starts_with('-') {
            return None;
        }
        [format!("refs/remotes/origin/{branch}^{{commit}}"), format!("refs/heads/{branch}^{{commit}}")]
            .iter()
            .find_map(|name| self.git(dir, &["rev-parse", "--verify", "--quiet", name]))
            .filter(|sha| !sha.is_empty())
    }

    /// The paths in `commit` that `paths` name, relative to `dir`: a
    /// folder with a trailing `/` lists the files in it.
    pub(crate) fn tree_files(&self, dir: &Path, commit: &str, paths: &[&str]) -> Vec<String> {
        let mut args = vec!["ls-tree", "--name-only", commit, "--"];
        args.extend_from_slice(paths);
        self.git(dir, &args).map(|out| out.lines().map(str::to_owned).collect()).unwrap_or_default()
    }

    /// The text of `path`, relative to `dir`, as it is in `commit`.
    pub(crate) fn show_file(&self, dir: &Path, commit: &str, path: &str) -> Option<String> {
        self.git(dir, &["show", &format!("{commit}:./{path}")])
    }

    /// Installs the `gh signoff` extension.
    ///
    /// # Errors
    ///
    /// A message for the user: `gh` could not install it.
    pub fn install_signoff_extension(&self) -> Result<(), String> {
        self.exec(None, "gh", &["extension", "install", SIGNOFF_EXTENSION]).map(drop).map_err(|err| {
            let reason = err.message().filter(|message| !message.is_empty()).unwrap_or("gh extension install failed");
            format!("could not install gh signoff: {reason}")
        })
    }
}

/// The repository `gh extension install` takes for `gh signoff`.
pub const SIGNOFF_EXTENSION: &str = "basecamp/gh-signoff";

/// The first eight characters of a commit id.
pub fn short_sha(sha: &str) -> &str {
    sha.get(..8).unwrap_or(sha)
}

/// Whether two sets of statuses agree on every signoff context and its
/// state, which decides whether a refresh learned anything that warrants a
/// second call for attribution.
pub fn same_contexts(a: &[CommitStatus], b: &[CommitStatus]) -> bool {
    use std::collections::BTreeMap;
    let index = |list: &[CommitStatus]| -> BTreeMap<String, CheckState> {
        list.iter().filter(|s| is_signoff_context(&s.context)).map(|s| (s.context.clone(), s.state)).collect()
    };
    index(a) == index(b)
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RawPr {
    number: u64,
    title: String,
    url: String,
    state: String,
    author: Option<Author>,
    head_ref_oid: String,
    head_ref_name: String,
    base_ref_name: String,
    mergeable: String,
    is_draft: bool,
    #[serde(deserialize_with = "timestamp_or_default")]
    updated_at: Timestamp,
    status_check_rollup: Option<Vec<RollupEntry>>,
}

#[derive(Debug, Default, Deserialize)]
struct Author {
    #[serde(default)]
    login: String,
}

/// One entry of `statusCheckRollup`. The list mixes Actions `CheckRun`s with
/// commit `StatusContext`s; only the latter can be a signoff.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RollupEntry {
    #[serde(rename = "__typename")]
    type_name: String,
    context: String,
    state: String,
    #[serde(deserialize_with = "timestamp_or_default")]
    started_at: Timestamp,
    target_url: Option<String>,
}

impl RawPr {
    /// The commit statuses in the rollup. GraphQL spells states in upper
    /// case; [`CheckState::parse`] normalises them.
    pub(crate) fn rollup_contexts(&self) -> Vec<CommitStatus> {
        self.status_check_rollup
            .iter()
            .flatten()
            .filter(|e| e.type_name == "StatusContext" && !e.context.is_empty())
            .map(|e| CommitStatus {
                context: e.context.clone(),
                state: CheckState::parse(&e.state),
                created_at: e.started_at,
                target_url: e.target_url.clone().unwrap_or_default(),
                ..CommitStatus::default()
            })
            .collect()
    }

    fn into_info(self) -> PrInfo {
        let contexts = self.rollup_contexts();
        PrInfo {
            number: self.number,
            title: self.title,
            author: self.author.map(|a| a.login).unwrap_or_default(),
            url: self.url,
            state: self.state,
            head_sha: self.head_ref_oid,
            head_ref: self.head_ref_name,
            base_ref: self.base_ref_name,
            mergeable: self.mergeable,
            is_draft: self.is_draft,
            updated_at: self.updated_at,
            contexts,
        }
    }
}

pub(crate) fn limit_pull_requests<T>(mut raw: Vec<T>) -> (Vec<T>, bool) {
    if raw.len() <= PULL_REQUEST_LIMIT {
        return (raw, false);
    }
    raw.truncate(PULL_REQUEST_LIMIT);
    (raw, true)
}

/// The statuses in a combined-status answer (`GET
/// /repos/{o}/{r}/commits/{sha}/status`), whether `gh api` or the REST
/// client fetched it.
pub(crate) fn parse_combined_status(body: &str) -> Result<Vec<CommitStatus>, serde_json::Error> {
    #[derive(Deserialize)]
    struct Combined {
        #[serde(default)]
        statuses: Vec<RawStatus>,
    }
    #[derive(Deserialize)]
    struct RawStatus {
        #[serde(default)]
        context: String,
        #[serde(default)]
        state: CheckState,
        #[serde(default)]
        description: Option<String>,
        #[serde(default, deserialize_with = "timestamp_or_default")]
        created_at: Timestamp,
        #[serde(default)]
        target_url: Option<String>,
        #[serde(default)]
        creator: Option<Creator>,
    }
    #[derive(Deserialize)]
    struct Creator {
        #[serde(default)]
        login: String,
        #[serde(default)]
        avatar_url: String,
    }
    let raw: Combined = serde_json::from_str(body)?;
    Ok(raw
        .statuses
        .into_iter()
        .map(|s| {
            let (creator, avatar_url) = s.creator.map(|c| (c.login, c.avatar_url)).unwrap_or_default();
            CommitStatus {
                context: s.context,
                state: s.state,
                description: s.description.unwrap_or_default(),
                creator,
                avatar_url,
                created_at: s.created_at,
                target_url: s.target_url.unwrap_or_default(),
            }
        })
        .collect())
}

/// A timestamp, or the epoch for `null` or a missing value.
pub(crate) fn timestamp_or_default<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Timestamp, D::Error> {
    Ok(Option::<Timestamp>::deserialize(deserializer)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::{resolve_checks, tally};
    use crate::runner::fake::{FakeRunner, fail, ok};
    use std::collections::BTreeMap;

    fn gh(runner: &Arc<FakeRunner>) -> Gh {
        Gh::new(runner.clone())
    }

    /// Counts `gh` reads running at once and the timeout each was given;
    /// the first `timeouts` of them time out.
    struct Slow {
        running: std::sync::Mutex<(usize, usize)>,
        timeouts: std::sync::Mutex<Vec<u64>>,
        time_out_first: usize,
    }

    impl Runner for Slow {
        fn run(&self, _: Option<&Path>, _: Option<&str>, _: &str, _: &[&str]) -> std::io::Result<Output> {
            unreachable!("reads give a timeout")
        }

        fn run_for(
            &self,
            _: Option<&Path>,
            _: Option<&str>,
            _: &str,
            _: &[&str],
            timeout: Duration,
        ) -> std::io::Result<Output> {
            let call = {
                let mut timeouts = self.timeouts.lock().expect("timeouts");
                timeouts.push(timeout.as_secs());
                timeouts.len()
            };
            {
                let mut running = self.running.lock().expect("running");
                running.0 += 1;
                running.1 = running.1.max(running.0);
            }
            std::thread::sleep(Duration::from_millis(20));
            self.running.lock().expect("running").0 -= 1;
            if call <= self.time_out_first {
                return Ok(Output { code: TIMED_OUT, stderr: "gh timed out".into(), ..Output::default() });
            }
            Ok(Output { stdout: "[]".into(), ..Output::default() })
        }
    }

    fn slow(time_out_first: usize) -> Arc<Slow> {
        Arc::new(Slow { running: std::sync::Mutex::new((0, 0)), timeouts: std::sync::Mutex::default(), time_out_first })
    }

    #[test]
    fn only_a_few_gh_reads_run_at_once() {
        let runner = slow(0);
        let gh = Gh::new(runner.clone());
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| gh.pull_requests(Path::new("/r")).expect("read"));
            }
        });
        let most = runner.running.lock().expect("running").1;
        assert!((1..=PARALLEL_GH_READS).contains(&most), "{most} at once");
    }

    #[test]
    fn reads_that_time_out_get_longer_until_one_finishes() {
        let runner = slow(3);
        let gh = Gh::new(runner.clone());
        for _ in 0..3 {
            assert!(gh.pull_requests(Path::new("/r")).is_err_and(|e| e.to_string().contains("timed out")));
        }
        gh.pull_requests(Path::new("/r")).expect("read");
        gh.pull_requests(Path::new("/r")).expect("read");
        assert_eq!(*runner.timeouts.lock().expect("timeouts"), [60, 120, 180, 180, 60]);
    }

    #[test]
    fn repo_uses_git_for_the_local_checkout() {
        let fake = Arc::new(FakeRunner::new(vec![
            ok("repo view", r#"{"nameWithOwner":"Asrat77/shakha","defaultBranchRef":{"name":"master"}}"#),
            ok("rev-parse --abbrev-ref", "dependabot/github_actions/actions/checkout-7"),
            ok("rev-parse HEAD", "b3c837bd2fae5f34633831c84cd45a856d99a76e"),
        ]));
        let info = gh(&fake).repo(Path::new("/repo")).expect("repo");
        assert_eq!(info.name_with_owner, "Asrat77/shakha");
        assert_eq!(info.default_branch, "master");
        assert_eq!(info.current_branch, "dependabot/github_actions/actions/checkout-7");
        assert_eq!(info.head_sha, "b3c837bd2fae5f34633831c84cd45a856d99a76e");
    }

    #[test]
    fn reads_one_pull_request_by_number() {
        let fake = Arc::new(FakeRunner::new(vec![ok(
            "pr view 482",
            r#"{"number":482,"title":"t","state":"OPEN","headRefOid":"3db401c9","headRefName":"f","baseRefName":"main"}"#,
        )]));
        let pr = gh(&fake).pull_request(Path::new("/repo"), 482).expect("pr");
        assert_eq!((pr.number, pr.head_sha.as_str(), pr.state.as_str()), (482, "3db401c9", "OPEN"));
        assert!(fake.called("--json number,title"));
    }

    /// The list must carry status state with it; a per-pull-request status
    /// call during a plain list would reintroduce the O(N) refresh cost.
    #[test]
    fn pull_requests_carry_rollup_state_and_cost_one_call() {
        let fake = Arc::new(FakeRunner::new(vec![ok(
            "pr list",
            r#"[{
            "number":1,"title":"Bump actions/checkout from 4 to 7","url":"https://github.com/Asrat77/shakha/pull/1",
            "state":"OPEN","author":{"login":"app/dependabot"},
            "headRefOid":"b3c837bd2fae5f34633831c84cd45a856d99a76e",
            "headRefName":"dependabot/github_actions/actions/checkout-7","baseRefName":"master",
            "mergeable":"MERGEABLE","isDraft":false,"updatedAt":"2026-08-10T08:18:52Z",
            "statusCheckRollup":[
                {"__typename":"CheckRun","name":"lint","conclusion":"FAILURE","status":"COMPLETED"},
                {"__typename":"StatusContext","context":"signoff/lint","state":"SUCCESS","startedAt":"2026-08-10T08:18:52Z","targetUrl":""},
                {"__typename":"StatusContext","context":"signoff/tests","state":"SUCCESS","startedAt":"2026-08-10T08:18:36Z","targetUrl":""}
            ]}]"#,
        )]));
        let (prs, truncated) = gh(&fake).pull_requests(Path::new("/repo")).expect("list");
        assert!(!truncated);
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].author, "app/dependabot");
        assert_eq!(prs[0].contexts.len(), 2, "the Actions CheckRun is not a signoff");
        assert!(prs[0].contexts.iter().all(|c| c.state == CheckState::Success));
        assert_eq!(fake.commands().len(), 1);
    }

    #[test]
    fn pull_requests_are_everyones_not_just_the_authors() {
        let fake = Arc::new(FakeRunner::new(vec![ok("pr list", "[]")]));
        gh(&fake).pull_requests(Path::new("/repo")).expect("list");
        assert!(!fake.called("--author"));
        assert!(fake.called("--state open"));
    }

    #[test]
    fn pull_request_limit_reports_truncation() {
        let (limited, truncated) = limit_pull_requests(vec![0; PULL_REQUEST_LIMIT + 1]);
        assert!(truncated);
        assert_eq!(limited.len(), PULL_REQUEST_LIMIT);
        let (limited, truncated) = limit_pull_requests(vec![0; PULL_REQUEST_LIMIT]);
        assert!(!truncated);
        assert_eq!(limited.len(), PULL_REQUEST_LIMIT);
    }

    /// A branch without a pull request is an ordinary state, not an error.
    #[test]
    fn no_pull_request_is_absent_not_an_error() {
        let fake = Arc::new(FakeRunner::new(vec![fail("pr view", "no pull requests found for branch \"feature\"")]));
        assert_eq!(gh(&fake).current_pull_request(Path::new("/repo")).expect("absent"), None);
    }

    /// Observed on a real signoff: the status API returned `creator: null`,
    /// and the name is only recoverable from the description.
    #[test]
    fn commit_statuses_survive_a_null_creator() {
        let fake = Arc::new(FakeRunner::new(vec![ok(
            "commits/",
            r#"{"statuses":[
            {"context":"signoff/tests","state":"success","description":"Asrat signed off",
             "created_at":"2026-08-10T08:18:36Z","target_url":null,"creator":null}]}"#,
        )]));
        let statuses = gh(&fake).commit_statuses(Path::new("/repo"), "Asrat77/shakha", "b3c837b").expect("statuses");
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].creator, "");
        assert_eq!(crate::checks::actor_from(&statuses[0]), "Asrat");
    }

    #[test]
    fn protection_distinguishes_absent_from_unreadable() {
        let cases = [
            (
                ok("protection", r#"["signoff/tests","signoff/lint"]"#),
                ProtectionStatus::Ok,
                vec!["signoff/tests", "signoff/lint"],
            ),
            (fail("protection", "gh: Branch not protected (HTTP 404)"), ProtectionStatus::Absent, vec![]),
            (fail("protection", "gh: Not Found (HTTP 404)"), ProtectionStatus::Unreadable, vec![]),
        ];
        for (reply, want, contexts) in cases {
            let fake = Arc::new(FakeRunner::new(vec![reply]));
            let (got, status) = gh(&fake).protection_contexts(Path::new("/repo"), "o/r", "main");
            assert_eq!(status, want);
            assert_eq!(got, contexts);
        }
    }

    /// The push-but-not-admin case: protection gates the merge but cannot be
    /// read. Readiness must fall back rather than claim a green it cannot
    /// verify.
    #[test]
    fn unreadable_protection_does_not_claim_readiness() {
        let fake = Arc::new(FakeRunner::new(vec![fail("protection", "gh: Not Found (HTTP 404)")]));
        let (_, status) = gh(&fake).protection_contexts(Path::new("/repo"), "o/r", "main");
        let checks = resolve_checks(&[], &BTreeMap::new(), status != ProtectionStatus::Unreadable);
        let t = tally(&checks);
        assert!(!t.ready);
        assert_eq!((t.passed, t.total), (0, 1));
    }

    #[test]
    fn probe_reports_missing_pieces() {
        let fake = Arc::new(FakeRunner::new(vec![
            ok("gh --version", "gh version 2.95.0 (2026-06-17)\nhttps://github.com/cli/cli"),
            ok("api user", r#"{"login":"Asrat77"}"#),
            ok("rev-parse --git-dir", ".git"),
            fail("signoff version", "unknown command"),
        ]));
        let env = gh(&fake).probe_environment(Some(Path::new("/repo")));
        assert!(env.gh_installed);
        assert_eq!(env.gh_version, "2.95.0 (2026-06-17)");
        assert!(env.authenticated);
        assert_eq!(env.login, "Asrat77");
        assert!(env.in_git_repo);
        assert!(!env.signoff_installed);
    }

    /// GraphQL spells states in upper case, unlike the REST API. Everything
    /// downstream compares lower case, so that must not leak out.
    #[test]
    fn environment_failures_are_told_apart_from_repository_ones() {
        let exit = |message: &str| GhError::Exit { command: "gh".into(), code: 1, message: message.into() };
        let launch = GhError::Launch { program: "gh".into(), source: std::io::ErrorKind::NotFound.into() };
        assert!(launch.is_environment_failure());
        assert!(exit("To get started with GitHub CLI, please run:  gh auth login").is_environment_failure());
        assert!(exit("HTTP 401: Bad credentials (https://api.github.com/user)").is_environment_failure());
        assert!(!exit("GraphQL: Could not resolve to a Repository").is_environment_failure());
        assert!(!exit("HTTP 404: Not Found").is_environment_failure());
    }

    #[test]
    fn rollup_contexts_normalise_state_and_drop_check_runs() {
        let entry = |type_name: &str, context: &str, state: &str| RollupEntry {
            type_name: type_name.into(),
            context: context.into(),
            state: state.into(),
            ..RollupEntry::default()
        };
        let raw = RawPr {
            status_check_rollup: Some(vec![
                entry("CheckRun", "", "SUCCESS"),
                entry("StatusContext", "signoff/tests", "SUCCESS"),
                entry("StatusContext", "signoff/lint", "FAILURE"),
                entry("StatusContext", "", "SUCCESS"),
            ]),
            ..RawPr::default()
        };
        let got = raw.rollup_contexts();
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].context.as_str(), got[0].state), ("signoff/tests", CheckState::Success));
        assert_eq!(got[1].state, CheckState::Failure);
    }

    #[test]
    fn same_contexts_notices_changes_and_ignores_noise() {
        let s = |context: &str, state| CommitStatus { context: context.into(), state, ..CommitStatus::default() };
        let base = [s("signoff/tests", CheckState::Success)];
        assert!(same_contexts(&base, &[s("signoff/tests", CheckState::Success)]));
        assert!(!same_contexts(&base, &[s("signoff/tests", CheckState::Failure)]));
        assert!(!same_contexts(
            &base,
            &[s("signoff/tests", CheckState::Success), s("signoff/lint", CheckState::Success)]
        ));
        assert!(!same_contexts(&base, &[]));
        assert!(same_contexts(&base, &[s("signoff/tests", CheckState::Success), s("ci/build", CheckState::Failure)]));
    }

    #[test]
    fn run_requires_the_pull_request_commit_to_be_checked_out() {
        let dir = tempfile::tempdir().expect("tempdir");
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git").args(args).current_dir(dir.path()).output().expect("git");
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.name=Signoff Test",
            "-c",
            "user.email=signoff@example.test",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ]);
        let sha = git(&["rev-parse", "HEAD"]);

        let gh = Gh::default();
        assert_eq!(gh.require_checked_out_commit(dir.path(), &sha), Ok(()));
        assert!(gh.require_checked_out_commit(dir.path(), &"0".repeat(40)).is_err());
    }

    #[test]
    fn checking_out_a_pull_request_runs_gh_in_a_clean_tree() {
        let fake = Arc::new(FakeRunner::new(vec![ok("status --porcelain", ""), ok("pr checkout 482", "")]));
        assert_eq!(gh(&fake).check_out_pull_request(Path::new("/repo"), 482), Ok(()));
        assert_eq!(
            fake.commands(),
            ["git status --porcelain --untracked-files=no", "gh pr checkout 482"],
            "tracked changes only: untracked files do not block a checkout"
        );
    }

    #[test]
    fn a_dirty_tree_is_refused_before_gh_runs() {
        let fake = Arc::new(FakeRunner::new(vec![ok("status --porcelain", " M src/main.rs\nA  new.rs")]));
        let err = gh(&fake).check_out_pull_request(Path::new("/repo"), 7).expect_err("dirty");
        assert!(err.contains("2 uncommitted changes") && err.contains("#7"), "{err}");
        assert!(!fake.called("pr checkout"), "gh must not run: {:?}", fake.commands());
    }

    #[test]
    fn checkout_failures_carry_gits_or_ghs_message() {
        let fake = Arc::new(FakeRunner::new(vec![fail("status --porcelain", "fatal: not a git repository")]));
        let err = gh(&fake).check_out_pull_request(Path::new("/repo"), 1).expect_err("git");
        assert!(err.contains("not a git repository"), "{err}");

        let fake = Arc::new(FakeRunner::new(vec![
            ok("status --porcelain", ""),
            fail("pr checkout", "error: Your local changes would be overwritten"),
        ]));
        let err = gh(&fake).check_out_pull_request(Path::new("/repo"), 1).expect_err("gh");
        assert!(err.contains("could not check out #1") && err.contains("would be overwritten"), "{err}");
    }

    #[test]
    fn installing_the_extension_runs_gh_extension_install() {
        let fake = Arc::new(FakeRunner::new(vec![ok("extension install", "")]));
        assert_eq!(gh(&fake).install_signoff_extension(), Ok(()));
        assert_eq!(fake.commands(), ["gh extension install basecamp/gh-signoff"]);
    }

    #[test]
    fn a_failed_install_carries_ghs_message() {
        let fake = Arc::new(FakeRunner::new(vec![fail("extension install", "could not resolve host")]));
        let err = gh(&fake).install_signoff_extension().expect_err("install");
        assert!(err.contains("could not install gh signoff") && err.contains("could not resolve host"), "{err}");
    }
}
