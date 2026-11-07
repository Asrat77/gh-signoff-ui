//! The whole state Signoff shows: every tracked repository's open pull
//! requests, their signoff checks, and recent activity.
//!
//! [`ProductService`] reads GitHub one repository at a time and patches each
//! result into the shared state as it lands; the `store` module explains how
//! those patches stay consistent with the app's own changes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard};
use std::thread;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::api::{Api, ApiError, Remote, path_segment};
use crate::checks::{
    self, Check, CheckState, Rules, display_name, is_signoff_context, latest_signoffs, resolve_checks, rules_for, tally,
};
use crate::config::{CommandMapping, ConfigStore, DetectedCommand, TrackedRepository};
use crate::files::GitTree;
use crate::gh::{CommitStatus, Environment, Gh, GhError, PrInfo, ProtectionStatus, RepoInfo, same_contexts};
use crate::history::{ActivityEvent, ActivityKind, HistoryStore};
use crate::store::{RepositoryRead, Store};

/// Repositories read at once.
pub const PARALLEL_REPOSITORIES: usize = 4;
/// Local activity entries merged into a [`ProductState`].
const LOCAL_HISTORY: usize = 200;

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
    /// Local commands by check name.
    pub commands: BTreeMap<String, CommandMapping>,
    /// Open pull requests shown.
    pub pull_request_count: usize,
    /// More were open than are shown.
    pub pull_requests_truncated: bool,
    /// Why it could not be read.
    pub error: Option<String>,
    /// It is read but has not been yet, so its pull requests are not shown.
    pub loading: bool,
    /// When it was last read; `None` before that, and while paused.
    pub fetched_at: Option<Timestamp>,
    /// The last read failed (see `error`), so the pull requests shown are
    /// the ones an earlier read found at this time. `None` when what is
    /// shown is current, or when nothing was ever read.
    pub stale_since: Option<Timestamp>,
}

impl RepositoryState {
    /// It is read, but what it has open is not known: not read yet, or
    /// every read so far failed. Its pull requests may be missing.
    pub fn unknown(&self) -> bool {
        self.enabled && (self.fetched_at.is_none() || (self.error.is_some() && self.stale_since.is_none()))
    }
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
    /// A read is running.
    pub refreshing: bool,
    /// A caveat to show.
    pub warning: Option<String>,
    /// What went wrong.
    pub error: Option<String>,
    /// When a repository was last read; `None` until the first one lands.
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

/// Reads the repository `tracked`: its checkout, open pull requests and
/// their checks. Also says whether a failure means `gh` is missing or signed
/// out, so the environment is probed again.
///
/// Reads go over the REST API when `gh` has one and a token, and through
/// `gh` subprocesses otherwise, or when the API cannot be reached.
pub(crate) fn read_repository(gh: &Gh, tracked: &TrackedRepository, cache: &StatusCache) -> (RepositoryRead, bool) {
    if let Some(api) = gh.api().filter(|api| api.usable()) {
        match read_with_api(gh, api, tracked) {
            Ok(read) => return read,
            Err(err) => log::debug!("reading {} through gh: {err}", tracked.name_with_owner),
        }
    }
    read_with_gh(gh, tracked, cache)
}

/// What a read knows before it reads anything.
fn initial_view(tracked: &TrackedRepository) -> RepositoryState {
    RepositoryState {
        id: tracked.id.clone(),
        path: tracked.path.clone(),
        name_with_owner: tracked.name_with_owner.clone(),
        default_branch: tracked.default_branch.clone(),
        enabled: tracked.enabled,
        commands: tracked.commands.clone(),
        ..RepositoryState::default()
    }
}

/// A read that ended in `err`.
///
/// A rate limit is not the repository's fault: the read is deferred, so
/// what it showed stays, and only a repository never read shows why.
fn failed(mut repository: RepositoryState, err: GhError) -> (RepositoryRead, bool) {
    let deferred = match &err {
        GhError::Api(ApiError::RateLimited { until }) => {
            repository.error = Some(rate_limit_message(*until));
            true
        }
        _ => {
            repository.error = Some(err.to_string());
            false
        }
    };
    let environment_failed = err.is_environment_failure();
    (RepositoryRead { repository, deferred, ..RepositoryRead::default() }, environment_failed)
}

/// What to tell the user while GitHub's rate limit stops reads.
pub(crate) fn rate_limit_message(until: Timestamp) -> String {
    let at = until.to_zoned(jiff::tz::TimeZone::system()).strftime("%H:%M");
    format!("GitHub's API rate limit is used up; Signoff reads again at {at}.")
}

/// Commit statuses read at once per repository over the API.
const PARALLEL_STATUSES: usize = 4;

/// Reads over the REST API. `Err` only when the API cannot be used at all
/// (no token, unreachable), for the caller to read through `gh` instead;
/// anything GitHub answered is a read, failed or not.
fn read_with_api(gh: &Gh, api: &Api, tracked: &TrackedRepository) -> Result<(RepositoryRead, bool), ApiError> {
    let mut view = initial_view(tracked);
    // Which repository and host: from gh, once per checkout per session.
    let remote = match api.remote(&tracked.path) {
        Some(remote) => remote,
        None => match gh.repo(&tracked.path) {
            Ok(info) => {
                let remote = Remote {
                    host: info.host,
                    name_with_owner: info.name_with_owner,
                    default_branch: info.default_branch,
                };
                api.remember(&tracked.path, remote.clone());
                remote
            }
            Err(err) => return Ok(failed(view, err)),
        },
    };
    let local = gh.local_head(&tracked.path);
    let info = RepoInfo {
        name_with_owner: remote.name_with_owner.clone(),
        host: remote.host.clone(),
        default_branch: remote.default_branch.clone(),
        current_branch: local.current_branch,
        head_sha: local.head_sha,
    };
    view.name_with_owner.clone_from(&info.name_with_owner);
    view.default_branch.clone_from(&info.default_branch);
    view.current_branch.clone_from(&info.current_branch);
    view.current_sha.clone_from(&info.head_sha);

    let (prs, truncated) = match api.pull_requests(&remote) {
        Ok(found) => found,
        Err(err) if err.falls_back() => return Err(err),
        Err(err) => {
            if err.is_status(404) || err.is_status(403) {
                // Renamed, moved, or access changed: resolve it again.
                api.forget(&tracked.path);
            }
            return Ok(failed(view, err.into()));
        }
    };
    view.pull_request_count = prs.len();
    view.pull_requests_truncated = truncated;

    let mut protection: HashMap<String, (Vec<String>, ProtectionStatus)> = HashMap::new();
    for pr in &prs {
        if protection.contains_key(&pr.base_ref) {
            continue;
        }
        match api.protection_contexts(&remote, &pr.base_ref) {
            Ok(found) => protection.insert(pr.base_ref.clone(), found),
            Err(err) if err.falls_back() => return Err(err),
            Err(err) => return Ok(failed(view, err.into())),
        };
    }

    let statuses = parallel(&prs, PARALLEL_STATUSES, |pr| api.commit_statuses(&remote, &pr.head_sha));
    let mut pull_requests = Vec::with_capacity(prs.len());
    let mut history = Vec::new();
    for (mut pr, statuses) in prs.into_iter().zip(statuses) {
        let (statuses, warning) = match statuses {
            Ok(statuses) => (statuses, None),
            Err(err) if err.falls_back() => return Err(err),
            // One pull request's statuses (a head force-pushed away between
            // the list and this call, say) must not hide the rest.
            Err(err @ (ApiError::Status { .. } | ApiError::Json { .. })) => {
                (Vec::new(), Some(format!("Could not read its commit statuses ({err}); showing none.")))
            }
            Err(err) => return Ok(failed(view, err.into())),
        };
        pr.contexts.clone_from(&statuses);
        let (contexts, status) = protection.get(&pr.base_ref).cloned().unwrap_or((Vec::new(), ProtectionStatus::Ok));
        let (mut state, events) = build_pull_request_state(tracked, &info, pr, contexts, status, statuses);
        if warning.is_some() {
            state.warning = warning;
        }
        pull_requests.push(state);
        history.extend(events);
    }
    Ok((RepositoryRead { repository: view, pull_requests, history, deferred: false }, false))
}

/// `f` applied to every item, `workers` at a time, in order.
fn parallel<T: Sync, R: Send>(items: &[T], workers: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<R>>> = Mutex::new(items.iter().map(|_| None).collect());
    thread::scope(|scope| {
        for _ in 0..workers.min(items.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else { break };
                    let result = f(item);
                    results.lock().unwrap_or_else(PoisonError::into_inner)[index] = Some(result);
                }
            });
        }
    });
    results.into_inner().unwrap_or_else(PoisonError::into_inner).into_iter().flatten().collect()
}

/// Reads through `gh` and `git` subprocesses.
fn read_with_gh(gh: &Gh, tracked: &TrackedRepository, cache: &StatusCache) -> (RepositoryRead, bool) {
    let mut view = initial_view(tracked);
    let info = match gh.repo(&tracked.path) {
        Ok(info) => info,
        Err(err) => return failed(view, err),
    };
    view.name_with_owner.clone_from(&info.name_with_owner);
    view.default_branch.clone_from(&info.default_branch);
    view.current_branch.clone_from(&info.current_branch);
    view.current_sha.clone_from(&info.head_sha);

    let (prs, truncated) = match gh.pull_requests(&tracked.path) {
        Ok(found) => found,
        Err(err) => return failed(view, err),
    };
    view.pull_request_count = prs.len();
    view.pull_requests_truncated = truncated;

    let mut protection: HashMap<String, (Vec<String>, ProtectionStatus)> = HashMap::new();
    let mut pull_requests = Vec::with_capacity(prs.len());
    let mut history = Vec::new();
    for pr in prs {
        let (contexts, status) = protection
            .entry(pr.base_ref.clone())
            .or_insert_with(|| {
                gh.protection_contexts(&tracked.path, &info.name_with_owner, &path_segment(&pr.base_ref))
            })
            .clone();
        // State comes from the rollup on the pull-request list, at no extra
        // call; the cache reaches for the status API only when something
        // changed.
        let statuses = cache.resolve(&info.name_with_owner, &pr.head_sha, &pr.contexts, || {
            gh.commit_statuses(&tracked.path, &info.name_with_owner, &pr.head_sha)
        });
        let (state, events) = build_pull_request_state(tracked, &info, pr, contexts, status, statuses);
        pull_requests.push(state);
        history.extend(events);
    }
    (RepositoryRead { repository: view, pull_requests, history, deferred: false }, false)
}

fn build_pull_request_state(
    tracked: &TrackedRepository,
    repo: &RepoInfo,
    pr: PrInfo,
    contexts: Vec<String>,
    protection: ProtectionStatus,
    statuses: Vec<CommitStatus>,
) -> (PullRequestState, Vec<ActivityEvent>) {
    let rules = rules_for(&pr.base_ref, contexts, protection);

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

/// Whether detection may set up `check`'s command: it has none, or the
/// one Signoff set up, and the person did not remove it.
fn wants_detection(tracked: &TrackedRepository, check: &str) -> bool {
    !tracked.dismissed.contains(check)
        && (tracked.commands.get(check).is_none_or(|m| m.command.trim().is_empty())
            || tracked.detected_source(check).is_some())
}

enum Source {
    GitHub {
        gh: Gh,
        config: Arc<ConfigStore>,
        history: Arc<HistoryStore>,
        seed_dir: Option<PathBuf>,
        cache: StatusCache,
        detections: Detections,
    },
    Demo,
}

/// The last commands detected per repository, so the CI config is read
/// again only when the default branch moves or other checks appear.
#[derive(Debug, Default)]
struct Detections(Mutex<HashMap<String, Detection>>);

impl Detections {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Detection>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Debug)]
struct Detection {
    commit: String,
    checks: BTreeSet<String>,
    found: BTreeMap<String, DetectedCommand>,
}

/// Keeps the latest [`ProductState`] and patches it as reads land.
///
/// Reading blocks on `gh`: call [`ProductService::plan_refresh`] and
/// [`ProductService::refresh_repository`] (or [`ProductService::refresh`],
/// which does both) from background threads, and read
/// [`ProductService::cached`] from the UI when [`ProductService::revision`]
/// moves. Local patches ([`ProductService::sync_config`],
/// [`ProductService::mark_signed_off`]) never block on `gh`.
pub struct ProductService {
    source: Source,
    store: RwLock<Store>,
    /// Bumped after every change to the state.
    revision: AtomicU64,
    /// Reads and probes running.
    active: AtomicUsize,
    /// One probe at a time.
    probing: Mutex<()>,
}

impl std::fmt::Debug for ProductService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProductService").field("demo", &self.is_demo()).finish_non_exhaustive()
    }
}

/// Counts a read or probe as running while it lives.
struct Busy<'a>(&'a ProductService);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
        // Clears `refreshing` once the last one ends.
        self.0.mutate(|_| ());
    }
}

impl ProductService {
    /// Reads GitHub for the repositories in `config`. `seed_dir` is probed
    /// for the environment when no repository is tracked yet.
    pub fn new(gh: Gh, config: Arc<ConfigStore>, history: Arc<HistoryStore>, seed_dir: Option<PathBuf>) -> Self {
        Self::from_source(Source::GitHub {
            gh,
            config,
            history,
            seed_dir,
            cache: StatusCache::default(),
            detections: Detections::default(),
        })
    }

    /// Serves [`crate::demo::product_state`] and never touches GitHub or disk.
    pub fn demo() -> Self {
        Self::from_source(Source::Demo)
    }

    fn from_source(source: Source) -> Self {
        Self {
            source,
            store: RwLock::new(Store::default()),
            revision: AtomicU64::new(0),
            active: AtomicUsize::new(0),
            probing: Mutex::new(()),
        }
    }

    /// Serves `state` as if just refreshed, for tests elsewhere in the crate.
    #[cfg(test)]
    pub(crate) fn with_state(state: ProductState) -> Self {
        let service = Self::demo();
        *service.store.write().unwrap_or_else(PoisonError::into_inner) = Store::serving(state);
        service
    }

    /// Whether this serves demo data.
    pub fn is_demo(&self) -> bool {
        matches!(self.source, Source::Demo)
    }

    /// Waits, for a bounded time, until config and history changes are on
    /// disk. Call it before the app exits.
    pub fn flush(&self) {
        if let Source::GitHub { config, history, .. } = &self.source {
            config.flush();
            history.flush();
        }
    }

    /// The config store, outside demo mode.
    pub fn config(&self) -> Option<&Arc<ConfigStore>> {
        match &self.source {
            Source::GitHub { config, .. } => Some(config),
            Source::Demo => None,
        }
    }

    /// The `gh` client, config and history, outside demo mode.
    pub(crate) fn github(&self) -> Option<(&Gh, &Arc<ConfigStore>, &Arc<HistoryStore>)> {
        match &self.source {
            Source::GitHub { gh, config, history, .. } => Some((gh, config, history)),
            Source::Demo => None,
        }
    }

    /// Starts tracking the checkout at `path`. Blocks on `gh`.
    ///
    /// # Errors
    ///
    /// In demo mode, or when [`ConfigStore::add_repository`] fails.
    pub fn add_repository(&self, path: &Path) -> Result<TrackedRepository, String> {
        let (gh, config, _) = self.github().ok_or("repositories cannot be added in demo mode")?;
        config.add_repository(gh, path).map_err(|err| err.to_string())
    }

    /// The latest state, without reading anything.
    pub fn cached(&self) -> ProductState {
        self.store().state.clone()
    }

    /// Moves every time the state changes, so a reader copies
    /// [`ProductService::cached`] only when there is something new.
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// Whether a read or probe is running.
    pub fn is_refreshing(&self) -> bool {
        self.active.load(Ordering::Acquire) > 0
    }

    /// Reads everything again and returns the new state, a few repositories
    /// at a time, patching each in as it lands. Blocks on `gh`.
    pub fn refresh(&self) -> ProductState {
        let ids = self.plan_refresh(false);
        let next = AtomicUsize::new(0);
        thread::scope(|scope| {
            for _ in 0..PARALLEL_REPOSITORIES.min(ids.len()) {
                scope.spawn(|| {
                    while let Some(id) = ids.get(next.fetch_add(1, Ordering::Relaxed)) {
                        self.refresh_repository(id);
                    }
                });
            }
        });
        self.cached()
    }

    /// Prepares a full refresh and returns the repositories it reads, for
    /// the caller to pass to [`ProductService::refresh_repository`]. Applies
    /// the config, and probes the environment (blocking on `gh`) when
    /// `recheck` asks for it or the cached probe no longer holds.
    pub fn plan_refresh(&self, recheck: bool) -> Vec<String> {
        let Source::GitHub { gh, config, seed_dir, .. } = &self.source else {
            // Demo data only changes through demo runs; a refresh must not
            // undo them.
            self.mutate(|store| {
                if store.state.fetched_at.is_none() {
                    *store = Store::serving(crate::demo::product_state(Timestamp::now()));
                }
            });
            return Vec::new();
        };
        let snapshot = config.snapshot();
        let dir = self.mutate(|store| {
            store.sync(&snapshot, config.path().to_owned());
            store.probe_dir(seed_dir.as_deref())
        });
        if recheck || self.store().needs_probe(&dir) {
            let _one_at_a_time = self.probing.lock().unwrap_or_else(PoisonError::into_inner);
            // Another caller may have probed while this one waited.
            if recheck || self.store().needs_probe(&dir) {
                let _busy = self.busy();
                let env = gh.probe_environment(dir.as_deref());
                self.mutate(|store| store.set_environment(env, dir));
            }
        }
        self.mutate(|store| {
            let ids = store.readable_ids();
            if ids.is_empty() {
                // Nothing to wait for: say so rather than "loading".
                store.state.fetched_at = Some(Timestamp::now());
            }
            ids
        })
    }

    /// Reads the repository `id` and patches it in. Blocks on `gh`. Returns
    /// whether the read landed: not when the repository is not read (paused,
    /// removed, gh signed out) or a newer read or local change overtook it.
    pub fn refresh_repository(&self, id: &str) -> bool {
        let Source::GitHub { gh, cache, .. } = &self.source else { return false };
        let _busy = self.busy();
        let Some((ticket, tracked)) = self.mutate(|store| store.begin_read(id)) else { return false };
        let (read, environment_failed) = read_repository(gh, &tracked, cache);
        let next_allowed = gh.next_allowed();
        self.mutate(|store| {
            if environment_failed {
                store.invalidate_environment();
            }
            store.set_rate_limit(next_allowed);
            store.land(&ticket, read, Timestamp::now())
        })
    }

    /// Sets up commands for the repository `id`'s checks from its CI
    /// config, as of its default branch: for checks that have no command,
    /// and for those whose command Signoff set up before. Commands the
    /// person typed or removed are left alone. Blocks on `git`; call it
    /// after [`ProductService::refresh_repository`], from the same thread.
    /// Returns whether the config changed.
    ///
    /// The default branch is read, never the checkout, so a pull request
    /// cannot choose the command that later runs for every other one.
    pub fn set_up_commands(&self, id: &str) -> bool {
        let Source::GitHub { gh, config, detections, .. } = &self.source else { return false };
        let Some(tracked) = config.find_repository(id).filter(|repo| repo.enabled) else { return false };
        let checks: BTreeSet<String> = self
            .store()
            .state
            .pull_requests
            .iter()
            .filter(|pr| pr.repo_id == id)
            .flat_map(|pr| pr.checks.iter().map(|check| check.name.clone()))
            .filter(|check| wants_detection(&tracked, check))
            .collect();
        if checks.is_empty() {
            return false;
        }
        let Some(commit) = gh.branch_commit(&tracked.path, &tracked.default_branch) else { return false };
        let cached =
            detections.lock().get(id).filter(|d| d.commit == commit && d.checks == checks).map(|d| d.found.clone());
        let found = cached.unwrap_or_else(|| {
            let names: Vec<&String> = checks.iter().collect();
            let found = crate::ci::detect(&GitTree::new(gh, &tracked.path, &commit), &names);
            detections.lock().insert(id.to_owned(), Detection { commit, checks, found: found.clone() });
            found
        });
        config.adopt_detected(id, &found).unwrap_or_else(|err| {
            log::warn!("could not save the commands set up for {}: {err}", tracked.name_with_owner);
            false
        })
    }

    /// When GitHub's rate limit lets reads start again, if it stopped them.
    /// Reads started before then do not reach GitHub, so a scheduler should
    /// wait for it.
    pub fn next_allowed(&self) -> Option<Timestamp> {
        match &self.source {
            Source::GitHub { gh, .. } => gh.next_allowed(),
            Source::Demo => None,
        }
    }

    /// Whether the repository `id` shows an error from its last read, for a
    /// scheduler to tell a failing read from one that landed.
    pub fn read_failed(&self, id: &str) -> bool {
        self.store().state.repositories.iter().any(|repo| repo.id == id && repo.error.is_some())
    }

    /// Applies the config to the state at once: a removed or paused
    /// repository's pull requests go, an added one shows as loading, and
    /// command mappings change in place. Returns the repositories to read
    /// for the rest. Never blocks on `gh`.
    pub fn sync_config(&self) -> Vec<String> {
        let Some(config) = self.config() else { return Vec::new() };
        let snapshot = config.snapshot();
        self.mutate(|store| store.sync(&snapshot, config.path().to_owned()))
    }

    /// Shows `check` signed off on `repo_id#number` at `head_sha` at once,
    /// after `gh signoff` succeeded; reads keep showing it until GitHub
    /// reports it too. Never blocks on `gh`.
    pub fn mark_signed_off(&self, repo_id: &str, number: u64, check: &str, head_sha: &str) {
        self.mutate(|store| {
            let actor = match store.state.env.login.as_str() {
                "" => "you".to_owned(),
                login => login.to_owned(),
            };
            store.mark_signed_off(repo_id, number, check, head_sha, &actor, Timestamp::now())
        });
    }

    /// The pull request with `number` in the repository with `repo_id`.
    pub fn find_pull_request(&self, repo_id: &str, number: u64) -> Option<PullRequestState> {
        self.store().state.pull_request(repo_id, number).cloned()
    }

    /// In demo mode, marks a check passed, as a successful run would. Does
    /// nothing outside demo mode.
    pub fn mark_demo_passed(&self, repo_id: &str, number: u64, check_name: &str) {
        if !self.is_demo() {
            return;
        }
        let now = Timestamp::now();
        self.mutate(|store| {
            if !store.mark_signed_off(repo_id, number, check_name, "", "you", now) {
                return;
            }
            let Some(pr) = store.state.pull_request(repo_id, number) else { return };
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
            store.state.history.insert(0, event);
            store.state.fetched_at = Some(now);
        });
    }

    fn store(&self) -> RwLockReadGuard<'_, Store> {
        self.store.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn busy(&self) -> Busy<'_> {
        self.active.fetch_add(1, Ordering::AcqRel);
        Busy(self)
    }

    /// Changes the store, re-sorts and re-summarises, and moves the revision
    /// if the state changed.
    fn mutate<T>(&self, change: impl FnOnce(&mut Store) -> T) -> T {
        // Read before locking the store, so the two locks never nest.
        let local = match &self.source {
            Source::GitHub { history, .. } => Some(history.snapshot(LOCAL_HISTORY)),
            Source::Demo => None,
        };
        let mut store = self.store.write().unwrap_or_else(PoisonError::into_inner);
        let before = store.state.clone();
        let value = change(&mut store);
        store.finish(local);
        store.state.refreshing = self.is_refreshing();
        if store.state != before {
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::fake::{FakeTransport, json, not_modified};
    use crate::config::{AppConfig, RepositoryUpdate};
    use crate::runner::fake::{FakeRunner, Reply, fail, ok};
    use crate::runner::{Runner, TIMED_OUT};

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

    /// `gh` output for one signed-in repository with two pull requests:
    /// #1 ready and checked out, #2 failing.
    fn one_repository() -> Vec<Reply> {
        vec![
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
        ]
    }

    fn tracked() -> TrackedRepository {
        TrackedRepository {
            id: "r1".into(),
            path: "/repo".into(),
            name_with_owner: "o/r".into(),
            enabled: true,
            commands: BTreeMap::from([(
                "tests".into(),
                CommandMapping { command: "cargo test".into(), timeout_seconds: 60 },
            )]),
            ..TrackedRepository::default()
        }
    }

    struct Fixture {
        service: Arc<ProductService>,
        config: Arc<ConfigStore>,
        dir: tempfile::TempDir,
    }

    fn fixture(runner: Arc<dyn Runner>, repos: Vec<TrackedRepository>) -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = Arc::new(ConfigStore::with_config(
            dir.path().join("config.json"),
            AppConfig { repositories: repos, ..AppConfig::default() },
        ));
        let history = Arc::new(HistoryStore::open(dir.path().join("history.json")).expect("history"));
        let service = Arc::new(ProductService::new(Gh::new(runner), config.clone(), history, None));
        Fixture { service, config, dir }
    }

    fn probes(fake: &FakeRunner) -> usize {
        fake.commands().iter().filter(|c| c.contains("gh --version")).count()
    }

    /// Answers like a [`FakeRunner`], but holds any command containing
    /// `gate` until [`Gated::open`], so a test can act while a read is in
    /// flight.
    struct Gated {
        inner: FakeRunner,
        gate: &'static str,
        open: Mutex<bool>,
        opened: std::sync::Condvar,
        entered: Mutex<std::sync::mpsc::Sender<()>>,
    }

    impl Gated {
        fn new(replies: Vec<Reply>, gate: &'static str) -> (Arc<Self>, std::sync::mpsc::Receiver<()>) {
            let (entered, waiting) = std::sync::mpsc::channel();
            let gated = Self {
                inner: FakeRunner::new(replies),
                gate,
                open: Mutex::new(false),
                opened: std::sync::Condvar::new(),
                entered: Mutex::new(entered),
            };
            (Arc::new(gated), waiting)
        }

        fn open(&self) {
            *self.open.lock().expect("gate") = true;
            self.opened.notify_all();
        }
    }

    impl Runner for Gated {
        fn run(
            &self,
            dir: Option<&Path>,
            input: Option<&str>,
            program: &str,
            args: &[&str],
        ) -> std::io::Result<crate::runner::Output> {
            if format!("{program} {}", args.join(" ")).contains(self.gate) {
                let _ = self.entered.lock().expect("entered").send(());
                let mut open = self.open.lock().expect("gate");
                while !*open {
                    open = self.opened.wait(open).expect("gate");
                }
            }
            self.inner.run(dir, input, program, args)
        }
    }

    /// One repository end to end against recorded `gh` output: a failing
    /// pull request sorts first and attribution comes from the status API.
    #[test]
    fn builds_state_from_gh() {
        let fake = Arc::new(FakeRunner::new(one_repository()));
        let f = fixture(fake.clone(), vec![tracked()]);
        let state = f.service.refresh();

        assert!(state.onboarded);
        assert!(state.fetched_at.is_some() && !state.refreshing);
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
        let repo = &state.repositories[0];
        assert!(!repo.loading && repo.fetched_at.is_some() && repo.pull_request_count == 2);
        assert!(state.history.iter().any(|e| e.kind == ActivityKind::Status && e.pr_number == 1));
        assert_eq!(
            fake.commands().iter().filter(|c| c.contains("protection")).count(),
            1,
            "protection is read once per base branch"
        );
    }

    /// The same repository as [`one_repository`], as the REST API answers
    /// it, with `ETag`s.
    fn rest_answers(transport: &FakeTransport) {
        transport
            .answer(
                "/pulls?",
                json(
                    200,
                    Some("\"pulls\""),
                    r#"[
                    {"number":1,"title":"Ready one","state":"open","head":{"sha":"aaa","ref":"feature"},
                     "base":{"ref":"main"},"user":{"login":"you","type":"User"},"updated_at":"2026-08-10T08:00:00Z"},
                    {"number":2,"title":"Failing one","state":"open","head":{"sha":"bbb","ref":"other"},
                     "base":{"ref":"main"},"user":{"login":"ann","type":"User"},"updated_at":"2026-08-09T08:00:00Z"}
                ]"#,
                ),
            )
            .answer(
                "/branches/main/protection",
                json(200, None, r#"{"required_status_checks":{"contexts":["signoff/tests","ci/build"]}}"#),
            )
            .answer(
                "/commits/aaa/status",
                json(
                    200,
                    Some("\"aaa\""),
                    r#"{"statuses":[{"context":"signoff/tests","state":"success","description":"you signed off",
                       "created_at":"2026-08-10T08:00:00Z","creator":{"login":"you","avatar_url":""}}]}"#,
                ),
            )
            .answer(
                "/commits/bbb/status",
                json(
                    200,
                    Some("\"bbb\""),
                    r#"{"statuses":[{"context":"signoff/tests","state":"failure","created_at":"2026-08-09T08:00:00Z"}]}"#,
                ),
            );
    }

    /// `gh` output for the API path: the probe, `gh repo view` once, and a
    /// token.
    fn api_gh(token: bool) -> Vec<Reply> {
        let mut replies = vec![
            ok("gh --version", "gh version 2.95.0"),
            ok("api user", r#"{"login":"you"}"#),
            ok("rev-parse --git-dir", ".git"),
            ok("signoff version", "gh-signoff 0.3.0"),
            ok(
                "repo view",
                r#"{"nameWithOwner":"o/r","defaultBranchRef":{"name":"main"},"url":"https://github.com/o/r"}"#,
            ),
            ok("rev-parse --abbrev-ref", "feature"),
            ok("rev-parse HEAD", "aaa"),
        ];
        replies.push(if token { ok("auth token", "gho_test") } else { fail("auth token", "not logged in") });
        replies
    }

    fn api_fixture(replies: Vec<Reply>) -> (Fixture, Arc<FakeRunner>, Arc<FakeTransport>) {
        let fake = Arc::new(FakeRunner::new(replies));
        let transport = Arc::new(FakeTransport::default());
        let f = fixture(fake.clone(), vec![tracked()]);
        let gh = Gh::with_api(fake.clone(), transport.clone());
        let service = Arc::new(ProductService::new(
            gh,
            f.config.clone(),
            Arc::new(HistoryStore::open(f.dir.path().join("h.json")).expect("history")),
            None,
        ));
        (Fixture { service, ..f }, fake, transport)
    }

    /// The same state as through `gh`, with no `gh` call but the one
    /// `gh repo view` and the token.
    #[test]
    fn reads_over_the_api_like_gh() {
        let (f, fake, transport) = api_fixture(api_gh(true));
        rest_answers(&transport);
        let state = f.service.refresh();

        assert_eq!(state.pull_requests.len(), 2);
        assert_eq!(state.pull_requests[0].pr.number, 2, "failing sorts first");
        assert_eq!(state.pull_requests[0].status, Health::Failing);
        let ready = &state.pull_requests[1];
        assert_eq!((ready.status, ready.checked_out, ready.pr.state.as_str()), (Health::Ready, true, "OPEN"));
        assert_eq!(ready.checks[0].actor, "you");
        assert!(ready.checks[0].configured);
        assert_eq!(ready.rules.signoff_contexts, ["signoff/tests"]);
        assert!(state.history.iter().any(|e| e.kind == ActivityKind::Status && e.pr_number == 1));
        assert!(!fake.called("pr list") && !fake.called("api repos/"), "nothing read through gh");
        assert_eq!(transport.sent()[0].header("Authorization"), Some("Bearer gho_test"));
    }

    /// Nothing changed: every resource answers 304 (protection is not even
    /// asked), nothing new lands, and gh is not asked again.
    #[test]
    fn an_unchanged_repository_costs_304s_and_lands_nothing_new() {
        let (f, fake, transport) = api_fixture(api_gh(true));
        rest_answers(&transport);
        let first = f.service.refresh();
        for (needle, etag) in [("/pulls?", "\"pulls\""), ("/commits/aaa/", "\"aaa\""), ("/commits/bbb/", "\"bbb\"")] {
            transport.answer(needle, not_modified(etag));
        }
        let second = f.service.refresh();

        assert_eq!(second.pull_requests, first.pull_requests, "not replaced: same fetched_at too");
        assert!(second.repositories[0].fetched_at >= first.repositories[0].fetched_at);
        assert_eq!(transport.count("/protection"), 1, "protection is kept");
        assert_eq!(transport.count("/pulls?"), 2);
        let conditional = transport.sent().iter().filter(|s| s.header("If-None-Match").is_some()).count();
        assert_eq!(conditional, 3);
        let gh_calls = |needle: &str| fake.commands().iter().filter(|c| c.contains(needle)).count();
        assert_eq!((gh_calls("repo view"), gh_calls("auth token")), (1, 1), "resolved once per session");
    }

    #[test]
    fn without_a_token_reads_go_through_gh() {
        let mut replies = api_gh(false);
        replies.extend(one_repository().into_iter().skip(4));
        let (f, fake, transport) = api_fixture(replies);
        let state = f.service.refresh();
        assert_eq!(state.pull_requests.len(), 2);
        assert!(fake.called("pr list"));
        assert!(transport.sent().is_empty());
    }

    #[test]
    fn an_unreachable_api_reads_through_gh() {
        let mut replies = api_gh(true);
        replies.extend(one_repository().into_iter().skip(4));
        let (f, fake, transport) = api_fixture(replies);
        transport.fail("/pulls?", "proxy refused the tunnel");
        let state = f.service.refresh();
        assert_eq!(state.pull_requests.len(), 2);
        assert!(fake.called("pr list"));
        assert_eq!(transport.sent().len(), 1);
    }

    /// The Mac this was found on: the API cannot be reached, and `gh`,
    /// which reads instead, times out. What was read before stays.
    #[test]
    fn when_the_api_is_unreachable_and_gh_times_out_the_last_read_stays() {
        let mut replies = api_gh(true);
        replies.push(Reply { matches: "pr list", stdout: "", stderr: "gh timed out after 60s", code: TIMED_OUT });
        let (f, fake, transport) = api_fixture(replies);
        rest_answers(&transport);
        let first = f.service.refresh();
        assert_eq!(first.pull_requests.len(), 2);

        transport.fail("/pulls?", "tls: unknown certificate authority");
        let second = f.service.refresh();
        assert!(fake.called("pr list"), "read through gh");
        assert_eq!(second.pull_requests, first.pull_requests, "nothing vanished");
        assert_eq!(second.summary, first.summary, "the counts hold");
        let repo = &second.repositories[0];
        assert!(repo.error.as_deref().is_some_and(|e| e.contains("timed out")));
        assert_eq!(repo.stale_since, first.repositories[0].fetched_at);
        assert!(f.service.read_failed("r1"), "the scheduler still backs off");
    }

    #[test]
    fn a_refused_token_asks_for_a_new_probe() {
        let (f, fake, transport) = api_fixture(api_gh(true));
        transport.answer("/pulls?", json(401, None, r#"{"message":"Bad credentials"}"#));
        let state = f.service.refresh();
        assert!(state.repositories[0].error.as_deref().is_some_and(|e| e.contains("gh auth login")));
        assert!(f.service.read_failed("r1") && !f.service.read_failed("elsewhere"));
        f.service.plan_refresh(false);
        assert_eq!(probes(&fake), 2);
    }

    /// One head whose statuses cannot be read must not hide the others.
    #[test]
    fn one_unreadable_head_warns_instead_of_failing_the_repository() {
        let (f, _, transport) = api_fixture(api_gh(true));
        transport
            .answer("/pulls?", json(200, None, r#"[{"number":1,"head":{"sha":"aaa"},"base":{"ref":"main"}},{"number":2,"head":{"sha":"gone"},"base":{"ref":"main"}}]"#))
            .answer("/protection", json(200, None, r#"{"required_status_checks":{"contexts":["signoff/tests"]}}"#))
            .answer("/commits/aaa/", json(200, None, r#"{"statuses":[{"context":"signoff/tests","state":"success"}]}"#))
            .answer("/commits/gone/", json(422, None, r#"{"message":"No commit found for SHA: gone"}"#));
        let state = f.service.refresh();
        assert_eq!(state.repositories[0].error, None);
        assert!(!f.service.read_failed("r1"));
        let gone = f.service.find_pull_request("r1", 2).expect("still listed");
        assert!(gone.warning.as_deref().is_some_and(|w| w.contains("No commit found")));
        assert_eq!(f.service.find_pull_request("r1", 1).expect("pr").status, Health::Ready);
    }

    fn rate_limited() -> crate::api::HttpResponse {
        let reset = Timestamp::now().as_second() + 600;
        crate::api::HttpResponse {
            status: 403,
            headers: vec![
                ("X-RateLimit-Remaining".into(), "0".into()),
                ("X-RateLimit-Reset".into(), reset.to_string()),
            ],
            body: r#"{"message":"API rate limit exceeded for user"}"#.into(),
        }
    }

    /// A rate limit keeps what is shown, says why nothing moves, and stops
    /// asking GitHub until it resets.
    #[test]
    fn a_rate_limit_keeps_the_last_read_and_warns() {
        let (f, _, transport) = api_fixture(api_gh(true));
        rest_answers(&transport);
        let first = f.service.refresh();
        assert_eq!((first.warning.as_ref(), f.service.next_allowed()), (None, None));

        transport.answer("/pulls?", rate_limited());
        let second = f.service.refresh();
        assert_eq!(second.pull_requests, first.pull_requests, "still shown");
        assert_eq!(second.repositories[0].error, None);
        assert!(second.warning.as_deref().is_some_and(|w| w.contains("rate limit")));
        assert!(f.service.next_allowed().is_some_and(|at| at > Timestamp::now()));

        let sent = transport.sent().len();
        f.service.refresh();
        assert_eq!(transport.sent().len(), sent, "nothing is sent until it resets");
    }

    #[test]
    fn a_rate_limit_before_the_first_read_says_why() {
        let (f, _, transport) = api_fixture(api_gh(true));
        transport.answer("/pulls?", rate_limited());
        let state = f.service.refresh();
        let repo = &state.repositories[0];
        assert!(!repo.loading, "an answer, not a spinner");
        assert!(repo.error.as_deref().is_some_and(|e| e.contains("reads again at")));
    }

    #[test]
    fn signed_out_skips_repositories() {
        let fake = Arc::new(FakeRunner::new(vec![
            ok("gh --version", "gh version 2.95.0"),
            fail("api user", "not logged in"),
            fail("rev-parse", "not a repo"),
            fail("signoff version", "unknown"),
        ]));
        let f = fixture(fake.clone(), vec![tracked()]);
        let state = f.service.refresh();
        assert!(!state.env.authenticated);
        assert!(state.repositories.is_empty());
        assert!(state.fetched_at.is_some(), "the setup problem shows instead of a spinner");
        assert!(!f.service.refresh_repository("r1"), "nothing is read while signed out");

        f.service.refresh();
        assert_eq!(probes(&fake), 2, "probed again until it is fixed");
    }

    #[test]
    fn the_probe_runs_once_until_asked_again() {
        let fake = Arc::new(FakeRunner::new(one_repository()));
        let f = fixture(fake.clone(), vec![tracked()]);
        f.service.refresh();
        f.service.refresh();
        assert_eq!(probes(&fake), 1, "not on every refresh");
        assert_eq!(fake.commands().iter().filter(|c| c.contains("pr list")).count(), 2);

        assert_eq!(f.service.plan_refresh(true), ["r1"]);
        assert_eq!(probes(&fake), 2, "on request");
    }

    #[test]
    fn the_probe_runs_again_after_gh_reports_a_sign_in_problem() {
        let fake = Arc::new(FakeRunner::new(vec![
            ok("gh --version", "gh version 2.95.0"),
            ok("api user", r#"{"login":"you"}"#),
            ok("rev-parse --git-dir", ".git"),
            ok("signoff version", "gh-signoff 0.3.0"),
            fail("repo view", "To get started with GitHub CLI, please run:  gh auth login"),
        ]));
        let f = fixture(fake.clone(), vec![tracked()]);
        let state = f.service.refresh();
        assert!(state.repositories[0].error.as_deref().is_some_and(|e| e.contains("gh auth login")));
        assert!(!state.repositories[0].loading, "an error is an answer");
        f.service.refresh();
        assert_eq!(probes(&fake), 2);
    }

    #[test]
    fn the_ui_sees_each_patch() {
        let fake = Arc::new(FakeRunner::new(one_repository()));
        let f = fixture(fake, vec![tracked()]);
        let start = f.service.revision();
        assert_eq!(f.service.plan_refresh(false), ["r1"]);
        let planned = f.service.revision();
        assert!(planned > start);
        let state = f.service.cached();
        assert!(state.repositories[0].loading && state.fetched_at.is_none(), "still looking");

        assert!(f.service.refresh_repository("r1"));
        assert!(f.service.revision() > planned);
        assert_eq!(f.service.cached().pull_requests.len(), 2);
        assert!(!f.service.is_refreshing());
    }

    #[test]
    fn adding_a_repository_shows_it_as_loading_until_read() {
        let fake = Arc::new(FakeRunner::new(one_repository()));
        let f = fixture(fake, Vec::new());
        let state = f.service.refresh();
        assert!(!state.onboarded && state.fetched_at.is_some());

        let added = f.service.add_repository(f.dir.path()).expect("add");
        assert_eq!(f.service.sync_config(), std::slice::from_ref(&added.id));
        let state = f.service.cached();
        assert!(state.onboarded && state.repositories[0].loading);
        assert_eq!(state.repositories[0].name_with_owner, "o/r");
        assert!(f.service.sync_config().is_empty(), "applied once");

        assert!(f.service.refresh_repository(&added.id));
        let state = f.service.cached();
        assert!(!state.repositories[0].loading);
        assert_eq!(state.pull_requests.len(), 2);
    }

    #[test]
    fn removing_a_repository_drops_its_pull_requests_at_once() {
        let fake = Arc::new(FakeRunner::new(one_repository()));
        let f = fixture(fake, vec![tracked()]);
        f.service.refresh();
        let before = f.service.revision();

        f.config.remove_repository("r1").expect("remove");
        assert!(f.service.sync_config().is_empty());
        let state = f.service.cached();
        assert!(state.repositories.is_empty() && state.pull_requests.is_empty() && !state.onboarded);
        assert_eq!(state.summary, ProductSummary::default());
        assert!(f.service.revision() > before);
        assert!(!f.service.refresh_repository("r1"));
    }

    #[test]
    fn pausing_while_a_read_is_in_flight_keeps_it_paused() {
        let (gated, entered) = Gated::new(one_repository(), "pr list");
        let f = fixture(gated.clone(), vec![tracked()]);
        assert_eq!(f.service.plan_refresh(false), ["r1"]);

        let service = f.service.clone();
        let read = std::thread::spawn(move || service.refresh_repository("r1"));
        entered.recv().expect("the read reached gh");
        assert!(f.service.is_refreshing());

        let paused = RepositoryUpdate { enabled: false, ..RepositoryUpdate::default() };
        f.config.update_repository("r1", paused).expect("pause");
        assert!(f.service.sync_config().is_empty());
        assert!(f.service.cached().repositories[0].fetched_at.is_none());

        gated.open();
        assert!(!read.join().expect("read"), "the read was overtaken");
        let state = f.service.cached();
        assert!(state.pull_requests.is_empty() && !state.repositories[0].enabled);
        assert!(!state.refreshing);
    }

    #[test]
    fn a_signoff_shows_at_once_and_a_lagging_read_keeps_it() {
        let (gated, entered) = Gated::new(one_repository(), "pr list");
        let f = fixture(gated.clone(), vec![tracked()]);
        gated.open();
        f.service.refresh();
        assert_eq!(f.service.find_pull_request("r1", 2).expect("pr").status, Health::Failing);

        // Read again, and sign off while that read is in flight.
        *gated.open.lock().expect("gate") = false;
        while entered.try_recv().is_ok() {}
        let service = f.service.clone();
        let read = std::thread::spawn(move || service.refresh_repository("r1"));
        entered.recv().expect("the read reached gh");
        f.service.mark_signed_off("r1", 2, "tests", "bbb");
        let pr = f.service.find_pull_request("r1", 2).expect("pr");
        assert_eq!((pr.status, pr.checks[0].actor.as_str()), (Health::Ready, "you"));
        assert_eq!(f.service.cached().summary.ready, 2);

        gated.open();
        assert!(read.join().expect("read"));
        assert_eq!(f.service.find_pull_request("r1", 2).expect("pr").status, Health::Ready, "GitHub lags");
        assert_eq!(f.service.cached().pull_requests[0].pr.number, 1, "re-sorted");
    }

    /// Runs `git` in `dir` for a test, failing it when git does.
    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn commands_are_set_up_from_the_default_branchs_ci_config_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".github/workflows")).expect("mkdir");
        let workflow =
            |command: &str| format!("on: pull_request\njobs:\n  test:\n    steps:\n      - run: {command}\n");
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join(".github/workflows/ci.yml"), workflow("cargo test")).expect("write");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "ci"]);
        // The checkout is on a pull request that changes what CI runs.
        git(&repo, &["checkout", "-qb", "pr"]);
        std::fs::write(repo.join(".github/workflows/ci.yml"), workflow("bin/steal-secrets")).expect("write");
        git(&repo, &["commit", "-qam", "change ci"]);

        let tracked = TrackedRepository {
            id: "r1".into(),
            path: repo.clone(),
            name_with_owner: "acme/repo".into(),
            default_branch: "main".into(),
            enabled: true,
            ..TrackedRepository::default()
        };
        let config = Arc::new(ConfigStore::with_config(
            dir.path().join("config.json"),
            AppConfig { repositories: vec![tracked], ..AppConfig::default() },
        ));
        let history = Arc::new(HistoryStore::open(dir.path().join("history.json")).expect("history"));
        let runner: Arc<dyn Runner> = Arc::new(crate::runner::SystemRunner::default());
        let service = ProductService::new(Gh::new(runner), config.clone(), history, None);
        let check = |name: &str| Check { name: name.into(), required: true, ..Check::default() };
        let pr = PullRequestState {
            repo_id: "r1".into(),
            checks: vec![check("tests"), check("deploy")],
            ..PullRequestState::default()
        };
        *service.store.write().unwrap_or_else(PoisonError::into_inner) =
            Store::serving(ProductState { pull_requests: vec![pr], ..ProductState::default() });

        assert!(service.set_up_commands("r1"));
        let saved = config.find_repository("r1").expect("repo");
        assert_eq!(saved.commands.keys().collect::<Vec<_>>(), ["tests"], "nothing is made up for deploy");
        assert_eq!(saved.commands["tests"].command, "cargo test", "read from main, not the checkout");
        assert_eq!(saved.detected_source("tests"), Some(".github/workflows/ci.yml"));
        assert!(!service.set_up_commands("r1"), "unchanged: nothing to save");
        assert!(!ProductService::demo().set_up_commands("r1"));
    }
}
