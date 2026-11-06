//! The state Signoff shows, kept current by patches rather than rebuilt.
//!
//! # Design
//!
//! [`Store`] owns the [`ProductState`] and is the only thing that changes
//! it. It does no I/O: [`crate::ProductService`] reads GitHub outside the
//! lock and hands the result to the store, which keeps reads and the app's
//! own changes from trampling each other.
//!
//! - **Per-repository reads.** A read starts with [`Store::begin_read`],
//!   which hands out a [`Ticket`], and ends with [`Store::land`], which
//!   patches in that one repository's pull requests. The UI can show each
//!   repository as soon as it lands instead of waiting for the slowest.
//! - **Epochs.** Every repository has an epoch, bumped when a read starts
//!   and by any local change that makes reads in flight wrong (removing or
//!   pausing). A ticket from an older
//!   epoch lands nothing, so a slow read never resurrects a repository the
//!   user just removed, and the newest read of a repository always wins.
//! - **Local patches.** [`Store::sync`] applies the config at once: removing
//!   or pausing a repository drops its pull requests, adding one shows a
//!   placeholder marked [`RepositoryState::loading`], and a changed command
//!   mapping is applied to the checks in place. It returns the
//!   repositories that need a read.
//! - **Optimistic signoffs.** [`Store::mark_signed_off`] shows a signoff the
//!   moment `gh signoff` succeeds and remembers it. Reads that land while
//!   GitHub has not caught up (one already in flight, or a list that lags
//!   the status API) keep showing it, until a read confirms it, the head
//!   commit moves, or [`OPTIMISTIC_FOR`] passes.
//! - **Environment probe.** The probe (`gh --version`, `gh api user`,
//!   `gh signoff version`) rarely changes, so its result is cached. It runs
//!   again on request, when the directory it would probe changes, while the
//!   last probe found a problem, and after a read fails in a way that says
//!   `gh` is missing or signed out.
//!
//! - **Unchanged reads.** A read that finds exactly what the repository
//!   already shows (the REST client's 304s make that the common case) only
//!   moves its `fetched_at`: the pull requests are not replaced, so nothing
//!   downstream sees a change. Reads still land in full while a signoff is
//!   shown optimistically, so it can expire.
//!
//! A scheduler only changes when reads start, and needs nothing here.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use jiff::{SignedDuration, Timestamp};
use sha2::{Digest as _, Sha256};

use crate::checks::CheckState;
use crate::config::{AppConfig, CommandMapping, TrackedRepository};
use crate::gh::Environment;
use crate::history::ActivityEvent;
use crate::product::{ProductState, PullRequestState, RepositoryState, summarize};

/// Activity entries kept in a [`ProductState`].
const MAX_HISTORY: usize = 300;

/// How long a signoff the app made is shown before GitHub confirms it.
pub(crate) const OPTIMISTIC_FOR: SignedDuration = SignedDuration::from_secs(120);

/// What one read of a repository found.
#[derive(Clone, Debug, Default)]
pub(crate) struct RepositoryRead {
    pub(crate) repository: RepositoryState,
    pub(crate) pull_requests: Vec<PullRequestState>,
    /// GitHub statuses, as activity.
    pub(crate) history: Vec<ActivityEvent>,
}

/// Permission to land a read of one repository, if nothing overtook it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ticket {
    repo_id: String,
    epoch: u64,
}

/// A signoff shown before GitHub reports it.
#[derive(Clone, Debug)]
struct Optimistic {
    repo_id: String,
    number: u64,
    head_sha: String,
    check: String,
    actor: String,
    at: Timestamp,
}

/// What the cached environment was probed for.
#[derive(Debug, Default)]
struct Probe {
    /// The directory probed; `None` until the first probe.
    probed_for: Option<Option<PathBuf>>,
    /// Probe again before the next full refresh.
    stale: bool,
}

/// The state and what it takes to patch it safely.
#[derive(Debug, Default)]
pub(crate) struct Store {
    pub(crate) state: ProductState,
    /// The repositories in the config, as last synced.
    tracked: Vec<TrackedRepository>,
    epochs: HashMap<String, u64>,
    /// Never reused, so a repository removed and added again starts fresh.
    last_epoch: u64,
    /// GitHub statuses per repository, merged into the history.
    statuses: HashMap<String, Vec<ActivityEvent>>,
    /// What the last read of each repository found, to tell an unchanged
    /// read from a new one.
    fingerprints: HashMap<String, [u8; 32]>,
    optimistic: Vec<Optimistic>,
    probe: Probe,
}

impl Store {
    /// A store serving `state` as is, for demo data.
    pub(crate) fn serving(state: ProductState) -> Self {
        Self { state, ..Self::default() }
    }

    /// The tracked repository `id`, if it is read at all.
    pub(crate) fn readable(&self, id: &str) -> Option<&TrackedRepository> {
        self.tracked.iter().find(|r| r.id == id && r.enabled).filter(|_| !self.environment_blocked())
    }

    /// Every repository a full refresh reads, in config order.
    pub(crate) fn readable_ids(&self) -> Vec<String> {
        if self.environment_blocked() {
            return Vec::new();
        }
        self.tracked.iter().filter(|r| r.enabled).map(|r| r.id.clone()).collect()
    }

    // --- The environment -------------------------------------------------

    /// Where the environment is probed: the first repository read, or
    /// `seed` when there is none.
    pub(crate) fn probe_dir(&self, seed: Option<&std::path::Path>) -> Option<PathBuf> {
        self.tracked.iter().find(|r| r.enabled).map(|r| r.path.clone()).or_else(|| seed.map(ToOwned::to_owned))
    }

    /// Whether the cached probe no longer holds for `dir`.
    pub(crate) fn needs_probe(&self, dir: &Option<PathBuf>) -> bool {
        self.probe.stale || self.probe.probed_for.as_ref() != Some(dir)
    }

    /// Probe again before the next full refresh.
    pub(crate) fn invalidate_environment(&mut self) {
        self.probe.stale = true;
    }

    /// Takes a new probe of `dir`. A probe that finds a problem stays stale,
    /// so every refresh checks again until the user fixes it.
    pub(crate) fn set_environment(&mut self, env: Environment, dir: Option<PathBuf>) {
        let was_blocked = self.environment_blocked();
        self.probe = Probe { probed_for: Some(dir), stale: !ready(&env) };
        self.state.env = env;
        if self.environment_blocked() && !was_blocked {
            // Nothing in flight may land while gh cannot be used.
            for id in self.tracked.iter().map(|r| r.id.clone()).collect::<Vec<_>>() {
                self.bump(&id);
            }
        }
        self.reconcile();
    }

    /// The last probe found gh missing or signed out: nothing is read.
    pub(crate) fn environment_blocked(&self) -> bool {
        self.probe.probed_for.is_some() && !ready(&self.state.env)
    }

    // --- Local patches ---------------------------------------------------

    /// Applies `config` at once and returns the repositories that need a
    /// read: added or resumed.
    pub(crate) fn sync(&mut self, config: &AppConfig, config_path: PathBuf) -> Vec<String> {
        self.state.config_path = config_path;
        let mut to_read = Vec::new();
        for old in &self.tracked {
            if !config.repositories.iter().any(|r| r.id == old.id) {
                self.epochs.remove(&old.id);
            }
        }
        for new in &config.repositories {
            let stale_reads = self
                .tracked
                .iter()
                .find(|old| old.id == new.id)
                .is_none_or(|old| old.enabled != new.enabled || old.path != new.path);
            if !stale_reads {
                continue;
            }
            self.bump(&new.id);
            if new.enabled {
                to_read.push(new.id.clone());
            }
        }
        self.tracked = config.repositories.clone();
        self.reconcile();
        if self.environment_blocked() {
            to_read.clear();
        }
        to_read
    }

    /// Shows `check` signed off on `repo_id#number` at `head_sha` (any head
    /// when empty) until GitHub says so too. False when that pull request,
    /// head or check is not shown.
    pub(crate) fn mark_signed_off(
        &mut self,
        repo_id: &str,
        number: u64,
        check: &str,
        head_sha: &str,
        actor: &str,
        now: Timestamp,
    ) -> bool {
        let Some(pr) = self.state.pull_requests.iter_mut().find(|pr| pr.repo_id == repo_id && pr.pr.number == number)
        else {
            return false;
        };
        if !head_sha.is_empty() && pr.pr.head_sha != head_sha {
            return false;
        }
        let mark = Optimistic {
            repo_id: repo_id.into(),
            number,
            head_sha: pr.pr.head_sha.clone(),
            check: check.into(),
            actor: actor.into(),
            at: now,
        };
        if !apply(&mark, pr) {
            return false;
        }
        self.optimistic.retain(|o| !(o.repo_id == repo_id && o.number == number && o.check == check));
        self.optimistic.push(mark);
        true
    }

    // --- Reads -----------------------------------------------------------

    /// Starts a read of `id`, overtaking any read of it in flight. `None`
    /// when it is not read at all.
    pub(crate) fn begin_read(&mut self, id: &str) -> Option<(Ticket, TrackedRepository)> {
        let tracked = self.readable(id)?.clone();
        let epoch = self.bump(id);
        Some((Ticket { repo_id: id.to_owned(), epoch }, tracked))
    }

    /// Patches in what a read found, unless something overtook it. Returns
    /// whether it landed.
    pub(crate) fn land(&mut self, ticket: &Ticket, read: RepositoryRead, now: Timestamp) -> bool {
        if self.epochs.get(&ticket.repo_id) != Some(&ticket.epoch) || self.readable(&ticket.repo_id).is_none() {
            return false;
        }
        let id = ticket.repo_id.clone();
        let fingerprint = fingerprint(&read);
        let shown = self.state.repositories.iter().any(|r| r.id == id && r.fetched_at.is_some());
        let unchanged = shown
            && self.fingerprints.get(&id) == Some(&fingerprint)
            && !self.optimistic.iter().any(|o| o.repo_id == id);
        if unchanged {
            if let Some(slot) = self.state.repositories.iter_mut().find(|r| r.id == id) {
                slot.fetched_at = Some(now);
            }
            self.state.fetched_at = Some(now);
            return true;
        }
        self.fingerprints.insert(id.clone(), fingerprint);
        let RepositoryRead { mut repository, mut pull_requests, history } = read;
        repository.fetched_at = Some(now);
        self.keep_optimistic(&id, &mut pull_requests, now);
        if let Some(slot) = self.state.repositories.iter_mut().find(|r| r.id == id) {
            *slot = repository;
        }
        self.state.pull_requests.retain(|pr| pr.repo_id != id);
        self.state.pull_requests.extend(pull_requests);
        self.statuses.insert(id, history);
        self.state.fetched_at = Some(now);
        self.reconcile();
        true
    }

    /// Re-applies the signoffs GitHub has not confirmed yet to a read of
    /// `repo_id`, forgetting those it confirmed, moved past or outlived.
    fn keep_optimistic(&mut self, repo_id: &str, prs: &mut [PullRequestState], now: Timestamp) {
        self.optimistic.retain(|mark| {
            if mark.repo_id != repo_id {
                return true;
            }
            let Some(pr) = prs.iter_mut().find(|pr| pr.pr.number == mark.number && pr.pr.head_sha == mark.head_sha)
            else {
                return false;
            };
            let confirmed = pr.checks.iter().any(|c| c.name == mark.check && c.state == CheckState::Success);
            if confirmed || now.duration_since(mark.at) > OPTIMISTIC_FOR {
                return false;
            }
            apply(mark, pr)
        });
    }

    // --- Derived state ---------------------------------------------------

    /// Makes the repositories match the tracked ones, in config order, and
    /// everything derived from them consistent.
    fn reconcile(&mut self) {
        self.state.onboarded = !self.tracked.is_empty();
        if self.environment_blocked() {
            self.state.repositories.clear();
            self.state.pull_requests.clear();
            self.statuses.clear();
            self.fingerprints.clear();
            self.optimistic.clear();
            return;
        }
        let previous = std::mem::take(&mut self.state.repositories);
        self.state.repositories = self
            .tracked
            .iter()
            .map(|tracked| {
                let mut repo = match previous.iter().find(|r| r.id == tracked.id) {
                    Some(repo) if tracked.enabled && repo.path == tracked.path => repo.clone(),
                    _ => placeholder(tracked),
                };
                repo.enabled = tracked.enabled;
                repo.commands.clone_from(&tracked.commands);
                repo.loading = tracked.enabled && repo.fetched_at.is_none();
                repo
            })
            .collect();

        let live: HashMap<&str, &BTreeMap<String, CommandMapping>> =
            self.tracked.iter().filter(|r| r.enabled).map(|r| (r.id.as_str(), &r.commands)).collect();
        self.state.pull_requests.retain(|pr| live.contains_key(pr.repo_id.as_str()));
        self.statuses.retain(|id, _| live.contains_key(id.as_str()));
        self.fingerprints.retain(|id, _| live.contains_key(id.as_str()));
        self.optimistic.retain(|o| live.contains_key(o.repo_id.as_str()));
        for pr in &mut self.state.pull_requests {
            if let Some(commands) = live.get(pr.repo_id.as_str()) {
                apply_commands(pr, commands);
            }
        }
        for repo in &mut self.state.repositories {
            if repo.fetched_at.is_some() {
                repo.pull_request_count = self.state.pull_requests.iter().filter(|pr| pr.repo_id == repo.id).count();
            }
        }
    }

    /// Re-sorts and re-summarises after a patch. `local` is the activity
    /// kept on disk; demo data, which has none, keeps its history.
    pub(crate) fn finish(&mut self, local: Option<Vec<ActivityEvent>>) {
        let state = &mut self.state;
        state.pull_requests.sort_by(|a, b| {
            a.status
                .rank()
                .cmp(&b.status.rank())
                .then_with(|| b.pr.updated_at.cmp(&a.pr.updated_at))
                .then_with(|| a.id.cmp(&b.id))
        });
        state.summary = summarize(&state.pull_requests);
        if let Some(local) = local {
            let mut history: Vec<ActivityEvent> = self.statuses.values().flatten().cloned().chain(local).collect();
            history.sort_by(|a, b| Reverse(a.at).cmp(&Reverse(b.at)).then_with(|| a.id.cmp(&b.id)));
            history.truncate(MAX_HISTORY);
            state.history = history;
        }
    }

    fn bump(&mut self, id: &str) -> u64 {
        self.last_epoch += 1;
        self.epochs.insert(id.to_owned(), self.last_epoch);
        self.last_epoch
    }
}

/// A digest of what `read` found, ignoring when it was read.
fn fingerprint(read: &RepositoryRead) -> [u8; 32] {
    let pull_requests: Vec<PullRequestState> = read
        .pull_requests
        .iter()
        .map(|pr| PullRequestState { fetched_at: Timestamp::default(), ..pr.clone() })
        .collect();
    let mut repository = read.repository.clone();
    repository.fetched_at = None;
    // Serialising these plain structs does not fail; if it did, the empty
    // digest would match and skip a land, so fall back to a unique one.
    match serde_json::to_vec(&(&repository, &pull_requests, &read.history)) {
        Ok(bytes) => Sha256::digest(bytes).into(),
        Err(_) => Sha256::digest(Timestamp::now().as_nanosecond().to_le_bytes()).into(),
    }
}

/// `gh` is installed and signed in.
fn ready(env: &Environment) -> bool {
    env.gh_installed && env.authenticated
}

/// A repository not read yet, or paused.
fn placeholder(tracked: &TrackedRepository) -> RepositoryState {
    RepositoryState {
        id: tracked.id.clone(),
        path: tracked.path.clone(),
        name_with_owner: tracked.name_with_owner.clone(),
        default_branch: tracked.default_branch.clone(),
        enabled: tracked.enabled,
        commands: tracked.commands.clone(),
        loading: tracked.enabled,
        ..RepositoryState::default()
    }
}

/// Marks `mark`'s check passed on `pr`; false if it has no such check.
fn apply(mark: &Optimistic, pr: &mut PullRequestState) -> bool {
    let Some(check) = pr.checks.iter_mut().find(|c| c.name == mark.check) else {
        return false;
    };
    check.state = CheckState::Success;
    check.actor.clone_from(&mark.actor);
    check.at = Some(mark.at);
    pr.retally();
    true
}

/// Shows the local command mapped to each check, or none.
fn apply_commands(pr: &mut PullRequestState, commands: &BTreeMap<String, CommandMapping>) {
    for check in &mut pr.checks {
        match commands.get(&check.name).filter(|m| !m.command.trim().is_empty()) {
            Some(mapping) => {
                check.local_command.clone_from(&mapping.command);
                check.configured = true;
            }
            None => {
                check.local_command.clear();
                check.configured = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::Check;
    use crate::gh::PrInfo;
    use crate::product::Health;

    fn at(minute: i64) -> Timestamp {
        Timestamp::from_second(1_800_000_000 + minute * 60).expect("time")
    }

    fn tracked(id: &str) -> TrackedRepository {
        TrackedRepository {
            id: id.into(),
            path: format!("/{id}").into(),
            name_with_owner: format!("o/{id}"),
            enabled: true,
            commands: BTreeMap::from([(
                "tests".into(),
                CommandMapping { command: "make test".into(), timeout_seconds: 60 },
            )]),
            ..TrackedRepository::default()
        }
    }

    fn config(repos: &[TrackedRepository]) -> AppConfig {
        AppConfig { repositories: repos.to_vec(), ..AppConfig::default() }
    }

    fn signed_in() -> Environment {
        Environment { gh_installed: true, authenticated: true, login: "me".into(), ..Environment::default() }
    }

    /// A store tracking `repos`, signed in as `me`.
    fn store(repos: &[TrackedRepository]) -> Store {
        let mut store = Store::default();
        store.sync(&config(repos), "/c/config.json".into());
        store.set_environment(signed_in(), Some("/r1".into()));
        store
    }

    fn pr(repo: &str, number: u64, tests: CheckState, author: &str, updated: i64) -> PullRequestState {
        let mut pr = PullRequestState {
            id: format!("{repo}:{number}"),
            repo_id: repo.into(),
            pr: PrInfo {
                number,
                author: author.into(),
                head_sha: format!("sha{number}"),
                updated_at: at(updated),
                ..PrInfo::default()
            },
            checks: vec![Check { name: "tests".into(), state: tests, required: true, ..Check::default() }],
            ..PullRequestState::default()
        };
        pr.retally();
        pr
    }

    fn read(repo: &str, prs: Vec<PullRequestState>) -> RepositoryRead {
        RepositoryRead {
            repository: RepositoryState {
                id: repo.into(),
                path: format!("/{repo}").into(),
                ..RepositoryState::default()
            },
            pull_requests: prs,
            history: Vec::new(),
        }
    }

    /// Starts and lands a read of `repo` finding `prs`.
    fn land(store: &mut Store, repo: &str, prs: Vec<PullRequestState>, now: Timestamp) -> bool {
        let (ticket, _) = store.begin_read(repo).expect("readable");
        let landed = store.land(&ticket, read(repo, prs), now);
        store.finish(Some(Vec::new()));
        landed
    }

    fn numbers(store: &Store) -> Vec<u64> {
        store.state.pull_requests.iter().map(|pr| pr.pr.number).collect()
    }

    fn tests_state(store: &Store, number: u64) -> CheckState {
        let pr = store.state.pull_requests.iter().find(|pr| pr.pr.number == number).expect("pr");
        pr.checks[0].state
    }

    #[test]
    fn each_repository_shows_as_it_lands_and_order_does_not_matter() {
        let repos = [tracked("r1"), tracked("r2")];
        let r1 = || vec![pr("r1", 1, CheckState::Success, "me", 5), pr("r1", 2, CheckState::Pending, "me", 1)];
        let r2 = || vec![pr("r2", 3, CheckState::Failure, "me", 2), pr("r2", 4, CheckState::Pending, "me", 9)];

        let mut a = store(&repos);
        assert!(a.state.repositories.iter().all(|r| r.loading), "placeholders until read");
        assert!(land(&mut a, "r1", r1(), at(10)));
        assert_eq!(numbers(&a), [2, 1], "the first repository is shown without waiting");
        assert_eq!(a.state.summary.total_pull_requests, 2);
        assert!(!a.state.repositories[0].loading && a.state.repositories[1].loading);
        assert!(land(&mut a, "r2", r2(), at(11)));

        let mut b = store(&repos);
        assert!(land(&mut b, "r2", r2(), at(10)));
        assert!(land(&mut b, "r1", r1(), at(11)));

        // Failing first, then attention by most recent, ready last.
        assert_eq!(numbers(&a), [3, 4, 2, 1]);
        assert_eq!(numbers(&a), numbers(&b));
        assert_eq!(a.state.summary, b.state.summary);
        assert_eq!(a.state.summary.status, Health::Failing);
        let ids = |s: &Store| s.state.repositories.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&b), ["r1", "r2"], "config order, not landing order");
        assert_eq!(b.state.repositories[0].pull_request_count, 2);
    }

    #[test]
    fn a_read_lands_only_its_own_repository() {
        let mut store = store(&[tracked("r1"), tracked("r2")]);
        land(&mut store, "r1", vec![pr("r1", 1, CheckState::Pending, "me", 0)], at(1));
        land(&mut store, "r2", vec![pr("r2", 2, CheckState::Pending, "me", 0)], at(2));
        land(&mut store, "r1", vec![pr("r1", 5, CheckState::Pending, "me", 0)], at(3));
        assert_eq!(numbers(&store), [5, 2], "r1's closed pull request went, r2's stayed");
        assert_eq!(store.state.fetched_at, Some(at(3)));
    }

    #[test]
    fn the_newest_read_wins_over_one_started_earlier() {
        let mut store = store(&[tracked("r1")]);
        let (older, _) = store.begin_read("r1").expect("readable");
        let (newer, _) = store.begin_read("r1").expect("readable");
        assert!(store.land(&newer, read("r1", vec![pr("r1", 2, CheckState::Success, "me", 0)]), at(2)));
        assert!(!store.land(&older, read("r1", vec![pr("r1", 1, CheckState::Pending, "me", 0)]), at(3)));
        assert_eq!(numbers(&store), [2]);
    }

    #[test]
    fn removing_a_repository_drops_it_at_once_and_its_read_in_flight() {
        let mut store = store(&[tracked("r1"), tracked("r2")]);
        land(&mut store, "r1", vec![pr("r1", 1, CheckState::Pending, "me", 0)], at(1));
        land(&mut store, "r2", vec![pr("r2", 2, CheckState::Pending, "me", 0)], at(1));
        let (in_flight, _) = store.begin_read("r1").expect("readable");

        assert!(store.sync(&config(&[tracked("r2")]), "/c".into()).is_empty(), "nothing new to read");
        store.finish(Some(Vec::new()));
        assert_eq!(numbers(&store), [2]);
        assert_eq!(store.state.repositories.len(), 1);
        assert_eq!(store.state.summary.total_pull_requests, 1);

        assert!(!store.land(&in_flight, read("r1", vec![pr("r1", 1, CheckState::Pending, "me", 0)]), at(2)));
        assert_eq!(numbers(&store), [2], "a slow read does not bring it back");

        // Tracked again: the old read still may not land, a new one does.
        assert_eq!(store.sync(&config(&[tracked("r2"), tracked("r1")]), "/c".into()), ["r1"]);
        assert!(store.state.repositories[1].loading);
        assert!(!store.land(&in_flight, read("r1", Vec::new()), at(3)));
        assert!(land(&mut store, "r1", vec![pr("r1", 9, CheckState::Pending, "me", 0)], at(4)));
    }

    #[test]
    fn pausing_drops_pull_requests_at_once_and_resuming_reads_again() {
        let mut store = store(&[tracked("r1")]);
        land(&mut store, "r1", vec![pr("r1", 1, CheckState::Pending, "me", 0)], at(1));
        let (in_flight, _) = store.begin_read("r1").expect("readable");

        let paused = TrackedRepository { enabled: false, ..tracked("r1") };
        assert!(store.sync(&config(std::slice::from_ref(&paused)), "/c".into()).is_empty());
        assert!(store.state.pull_requests.is_empty());
        let repo = &store.state.repositories[0];
        assert!(!repo.enabled && !repo.loading && repo.fetched_at.is_none());
        assert!(store.begin_read("r1").is_none(), "a paused repository is not read");
        assert!(!store.land(&in_flight, read("r1", vec![pr("r1", 1, CheckState::Pending, "me", 0)]), at(2)));
        assert!(store.state.pull_requests.is_empty());

        assert_eq!(store.sync(&config(&[tracked("r1")]), "/c".into()), ["r1"]);
        assert!(store.state.repositories[0].loading);
    }

    #[test]
    fn a_new_command_mapping_applies_in_place_without_a_read() {
        let mut store = store(&[tracked("r1")]);
        land(&mut store, "r1", vec![pr("r1", 1, CheckState::Pending, "me", 0)], at(1));
        assert!(store.state.pull_requests[0].checks[0].configured);

        let unmapped = TrackedRepository { commands: BTreeMap::new(), ..tracked("r1") };
        assert!(store.sync(&config(&[unmapped]), "/c".into()).is_empty());
        let check = &store.state.pull_requests[0].checks[0];
        assert!(!check.configured && check.local_command.is_empty());
        assert!(store.state.repositories[0].commands.is_empty());
    }

    #[test]
    fn a_signoff_shows_at_once_and_survives_a_read_that_lags() {
        let mut store = store(&[tracked("r1")]);
        land(&mut store, "r1", vec![pr("r1", 1, CheckState::Pending, "me", 0)], at(0));
        let (in_flight, _) = store.begin_read("r1").expect("readable");

        assert!(store.mark_signed_off("r1", 1, "tests", "sha1", "me", at(1)));
        store.finish(Some(Vec::new()));
        assert_eq!(tests_state(&store, 1), CheckState::Success);
        assert_eq!(store.state.summary.ready, 1, "re-summarised");

        // A read that started before the signoff, then one that lags it.
        assert!(store.land(&in_flight, read("r1", vec![pr("r1", 1, CheckState::Pending, "me", 0)]), at(1)));
        assert_eq!(tests_state(&store, 1), CheckState::Success);
        land(&mut store, "r1", vec![pr("r1", 1, CheckState::Pending, "me", 0)], at(2));
        assert_eq!(tests_state(&store, 1), CheckState::Success);

        // Confirmed: from now on GitHub is believed, even if it changes.
        land(&mut store, "r1", vec![pr("r1", 1, CheckState::Success, "me", 0)], at(3));
        land(&mut store, "r1", vec![pr("r1", 1, CheckState::Failure, "me", 0)], at(4));
        assert_eq!(tests_state(&store, 1), CheckState::Failure);
    }

    #[test]
    fn an_unchanged_read_only_moves_fetched_at() {
        let mut store = store(&[tracked("r1")]);
        let found = || vec![pr("r1", 1, CheckState::Pending, "me", 0)];
        land(&mut store, "r1", found(), at(0));
        // Something the read never touches, to see the shown list stay put.
        store.state.pull_requests[0].warning = Some("kept".into());

        assert!(land(&mut store, "r1", found(), at(1)), "an unchanged read still lands");
        assert_eq!(store.state.repositories[0].fetched_at, Some(at(1)));
        assert_eq!(store.state.fetched_at, Some(at(1)));
        assert_eq!(store.state.pull_requests[0].warning.as_deref(), Some("kept"), "not replaced");

        land(&mut store, "r1", vec![pr("r1", 1, CheckState::Success, "me", 0)], at(2));
        assert_eq!(tests_state(&store, 1), CheckState::Success, "a change replaces it");
        assert_eq!(store.state.pull_requests[0].warning, None);
    }

    #[test]
    fn unchanged_reads_land_in_full_after_a_pause_or_while_a_signoff_waits() {
        let mut store = store(&[tracked("r1")]);
        let found = || vec![pr("r1", 1, CheckState::Pending, "me", 0)];
        land(&mut store, "r1", found(), at(0));

        let mut paused = tracked("r1");
        paused.enabled = false;
        store.sync(&config(&[paused]), "/c/config.json".into());
        store.sync(&config(&[tracked("r1")]), "/c/config.json".into());
        assert!(store.state.pull_requests.is_empty());
        land(&mut store, "r1", found(), at(1));
        assert_eq!(numbers(&store), [1], "the same read shows again after resuming");

        assert!(store.mark_signed_off("r1", 1, "tests", "sha1", "me", at(1)));
        let later = at(1) + OPTIMISTIC_FOR + SignedDuration::from_secs(1);
        land(&mut store, "r1", found(), later);
        assert_eq!(tests_state(&store, 1), CheckState::Pending, "the expired signoff goes");
    }

    #[test]
    fn a_signoff_is_forgotten_when_the_head_moves_or_it_outlives_its_welcome() {
        let mut store = store(&[tracked("r1")]);
        land(
            &mut store,
            "r1",
            vec![pr("r1", 1, CheckState::Pending, "me", 0), pr("r1", 2, CheckState::Pending, "me", 0)],
            at(0),
        );
        assert!(!store.mark_signed_off("r1", 1, "tests", "elsewhere", "me", at(0)), "not the shown head");
        assert!(!store.mark_signed_off("r1", 1, "lint", "sha1", "me", at(0)), "no such check");
        assert!(store.mark_signed_off("r1", 1, "tests", "sha1", "me", at(0)));
        assert!(store.mark_signed_off("r1", 2, "tests", "sha2", "me", at(0)));

        let mut moved = pr("r1", 1, CheckState::Pending, "me", 0);
        moved.pr.head_sha = "new".into();
        let later = at(0) + OPTIMISTIC_FOR + SignedDuration::from_secs(1);
        land(&mut store, "r1", vec![moved, pr("r1", 2, CheckState::Pending, "me", 0)], later);
        assert_eq!(tests_state(&store, 1), CheckState::Pending, "a new head needs a new signoff");
        assert_eq!(tests_state(&store, 2), CheckState::Pending, "expired");
    }

    #[test]
    fn signed_out_shows_nothing_and_reads_nothing() {
        let mut store = store(&[tracked("r1")]);
        land(&mut store, "r1", vec![pr("r1", 1, CheckState::Pending, "me", 0)], at(0));
        let (in_flight, _) = store.begin_read("r1").expect("readable");

        store.set_environment(Environment { gh_installed: true, ..Environment::default() }, Some("/r1".into()));
        assert!(store.environment_blocked());
        assert!(store.state.repositories.is_empty() && store.state.pull_requests.is_empty());
        assert!(store.state.onboarded);
        assert!(store.readable_ids().is_empty() && store.begin_read("r1").is_none());
        assert!(!store.land(&in_flight, read("r1", vec![pr("r1", 1, CheckState::Pending, "me", 0)]), at(1)));
        assert!(store.needs_probe(&Some("/r1".into())), "checked again until it is fixed");

        store.set_environment(signed_in(), Some("/r1".into()));
        assert_eq!(store.readable_ids(), ["r1"]);
        assert!(store.state.repositories[0].loading);
    }

    #[test]
    fn the_probe_is_cached_until_invalidated_or_its_directory_changes() {
        let mut store = Store::default();
        let here = Some(PathBuf::from("/r1"));
        assert!(store.needs_probe(&here), "never probed");
        store.set_environment(signed_in(), here.clone());
        assert!(!store.needs_probe(&here));
        assert!(store.needs_probe(&Some("/elsewhere".into())));
        assert!(store.needs_probe(&None));
        store.invalidate_environment();
        assert!(store.needs_probe(&here));
    }

    #[test]
    fn the_probe_follows_the_first_repository_read() {
        let paused = TrackedRepository { enabled: false, ..tracked("r1") };
        let mut store = Store::default();
        let seed = std::path::Path::new("/seed");
        assert_eq!(store.probe_dir(Some(seed)), Some(seed.to_owned()));
        store.sync(&config(&[paused, tracked("r2")]), "/c".into());
        assert_eq!(store.probe_dir(Some(seed)), Some(PathBuf::from("/r2")));
    }
}
