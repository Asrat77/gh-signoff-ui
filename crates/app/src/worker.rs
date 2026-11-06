//! The background threads that keep the state current.
//!
//! Reading GitHub blocks on the network or `gh`, so none of it runs on the
//! UI thread. Two kinds of thread share the work:
//!
//! - **The scheduler** decides when to read. Some things read at once: the
//!   app starting, Refresh (and the popover opening on stale data), the
//!   config changing a repository, a signoff landing on one, and a
//!   checkout's `HEAD` or branches changing on disk ([`GitWatcher`],
//!   debounced). Between those, full reads follow [`cadence::wait`]: often
//!   while the popover is open or something is in flight, rarely when
//!   nothing is open, backing off after failures and never before GitHub's
//!   rate limit lifts. Waking from sleep (the clocks disagree about how long
//!   a wait took) rechecks everything. [`Schedule`] holds those decisions
//!   with the clock passed in, so they are tested without waiting.
//! - **Readers**, a few of them, take jobs from a queue that drops
//!   duplicates and puts single-repository reads first. A full refresh is a
//!   plan job (sync the config, probe the environment if needed) that
//!   queues one read per repository, so repositories land one by one. Each
//!   read reports whether it failed, and the reader that empties the queue
//!   says so, which is when the scheduler settles the outcome and times the
//!   next read.
//!
//! [`ProductService`] patches each read into the state and moves its
//! revision; the readers wake the UI after every job, and the UI copies the
//! state when the revision moved. [`Pulse`] is what the scheduler knows
//! about how reads are going, for the popover's live indicator.

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use signoff_core::product::PARALLEL_REPOSITORIES;
use signoff_core::{CheckState, ProductService, ProductState};

use crate::cadence::{self, Signals};
use crate::watcher::GitWatcher;

type Wake = Arc<dyn Fn() + Send + Sync>;

/// What the scheduler is told.
#[derive(Debug)]
enum Command {
    /// Read everything; `recheck` probes the environment again too.
    Refresh {
        recheck: bool,
    },
    /// Read one repository soon.
    Repository(String),
    /// The config changed: patch the state, then read what needs it.
    ConfigChanged,
    /// The app's window opened or closed, or a local run started or ended.
    Activity {
        visible: bool,
        running: bool,
    },
    /// These repositories' checkouts changed on disk.
    GitChanged(Vec<String>),
    /// A read finished: whether it failed, or `None` when it says nothing
    /// about the connection (overtaken, or rate limited).
    Read(Option<bool>),
    /// The queue ran empty: everything asked for has been read.
    Idle,
    Quit,
}

/// One unit of work for a reader.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Job {
    /// Prepare a full refresh and queue its reads.
    Plan { recheck: bool },
    /// Read one repository.
    Read(String),
}

/// Jobs waiting and running.
#[derive(Debug, Default)]
struct Jobs {
    waiting: VecDeque<Job>,
    running: usize,
    /// A plan is running; another waits for it.
    planning: bool,
    quit: bool,
}

impl Jobs {
    /// Queues a full refresh, merged into one already waiting.
    fn plan(&mut self, recheck: bool) {
        for job in &mut self.waiting {
            if let Job::Plan { recheck: waiting } = job {
                *waiting |= recheck;
                return;
            }
        }
        self.waiting.push_back(Job::Plan { recheck });
    }

    /// Queues a read of `id` unless one is waiting; an `urgent` one goes
    /// first.
    fn read(&mut self, id: String, urgent: bool) {
        if let Some(at) = self.waiting.iter().position(|job| matches!(job, Job::Read(waiting) if *waiting == id)) {
            if !urgent {
                return;
            }
            self.waiting.remove(at);
        }
        if urgent {
            self.waiting.push_front(Job::Read(id));
        } else {
            self.waiting.push_back(Job::Read(id));
        }
    }

    /// The next job to run, if any may run now.
    fn take(&mut self) -> Option<Job> {
        let at = self.waiting.iter().position(|job| !(self.planning && matches!(job, Job::Plan { .. })))?;
        let job = self.waiting.remove(at)?;
        self.running += 1;
        self.planning |= matches!(job, Job::Plan { .. });
        Some(job)
    }

    fn finish(&mut self, job: &Job) {
        self.running -= 1;
        if matches!(job, Job::Plan { .. }) {
            self.planning = false;
        }
    }

    fn busy(&self) -> bool {
        self.running > 0 || !self.waiting.is_empty()
    }
}

/// The jobs, shared by the scheduler and the readers.
#[derive(Default)]
struct Queue {
    jobs: Mutex<Jobs>,
    changed: Condvar,
}

impl Queue {
    fn lock(&self) -> MutexGuard<'_, Jobs> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Changes the jobs and lets the readers look again.
    fn update(&self, change: impl FnOnce(&mut Jobs)) {
        change(&mut self.lock());
        self.changed.notify_all();
    }

    /// Waits for a job; `None` once quitting.
    fn wait(&self) -> Option<Job> {
        let mut jobs = self.lock();
        loop {
            if jobs.quit {
                return None;
            }
            if let Some(job) = jobs.take() {
                return Some(job);
            }
            jobs = self.changed.wait(jobs).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// How reads are going, as the scheduler sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pulse {
    /// Rounds of reads in a row in which every read failed.
    pub(crate) failures: u32,
    /// When a round of reads last succeeded.
    pub(crate) last_ok: Option<Timestamp>,
    /// When the next timed read starts.
    pub(crate) next_read: Option<Instant>,
}

/// The longest the scheduler sleeps at once, so it notices the clock
/// jumping (the computer slept) soon after waking.
const TICK: Duration = Duration::from_secs(15);

/// A checkout's changes are read once they have been quiet this long...
const GIT_QUIET: Duration = Duration::from_millis(300);
/// ...or this long after the first, if they keep coming (a rebase).
const GIT_MAX_DELAY: Duration = Duration::from_secs(2);

/// Demo data never changes on its own: no timed reads.
const UNTIMED: Duration = Duration::from_secs(24 * 3600);

/// When to read next, with the clock passed in.
#[derive(Debug)]
struct Schedule {
    /// Timed reads happen at all.
    timed: bool,
    signals: Signals,
    failures: u32,
    last_ok: Option<Timestamp>,
    /// Since the last settle: some read succeeded, some read failed.
    succeeded: bool,
    failed: bool,
    /// A full read was asked for since the last settle.
    full: bool,
    settled_at: Instant,
    /// When the next timed full read starts.
    next: Instant,
    /// Repositories whose checkouts changed, waiting for the changes to
    /// stop, with when the first and the latest came.
    git: BTreeSet<String>,
    git_first: Option<Instant>,
    git_last: Option<Instant>,
}

impl Schedule {
    /// The schedule as the app starts: the first full read is on its way.
    fn new(now: Instant, timed: bool) -> Self {
        let mut schedule = Self {
            timed,
            signals: Signals::default(),
            failures: 0,
            last_ok: None,
            succeeded: false,
            failed: false,
            full: true,
            settled_at: now,
            next: now,
            git: BTreeSet::new(),
            git_first: None,
            git_last: None,
        };
        schedule.next = now + schedule.wait(None);
        schedule
    }

    fn wait(&self, rate_limited_for: Option<Duration>) -> Duration {
        if self.timed { cadence::wait(self.signals, self.failures, rate_limited_for) } else { UNTIMED }
    }

    /// A full read was queued: wait for it to settle, but read again on
    /// time even if it never reports back.
    fn started_full(&mut self, now: Instant, rate_limited_for: Option<Duration>) {
        self.full = true;
        self.next = now + self.wait(rate_limited_for);
    }

    /// One read finished; `None` says nothing about the connection.
    fn record(&mut self, failed: Option<bool>) {
        match failed {
            Some(true) => self.failed = true,
            Some(false) => self.succeeded = true,
            None => {}
        }
    }

    /// Everything asked for has been read: counts a failure if every read
    /// since the last settle failed, success otherwise, and times the next
    /// full read from now. A repository that cannot be read among others
    /// that can is that repository's problem, not the connection's.
    fn settle(&mut self, now: Instant, wall: Timestamp, state: &ProductState, rate_limited_for: Option<Duration>) {
        if self.failed && !self.succeeded {
            self.failures = self.failures.saturating_add(1);
        } else {
            self.failures = 0;
            self.last_ok = Some(wall);
        }
        (self.succeeded, self.failed) = (false, false);
        self.signals.open = !state.pull_requests.is_empty();
        self.signals.pending =
            state.pull_requests.iter().any(|pr| pr.checks.iter().any(|check| check.state == CheckState::Pending));
        let next = now + self.wait(rate_limited_for);
        // Reading one repository does not put off reading them all.
        self.next = if self.full { next } else { self.next.min(next) };
        self.full = false;
        self.settled_at = now;
    }

    /// The window opened or closed, or a run started or ended: retimes the
    /// next read from the last settle, unless one is on its way.
    fn set_activity(&mut self, visible: bool, running: bool, rate_limited_for: Option<Duration>) {
        self.signals.visible = visible;
        self.signals.running = running;
        if !self.full {
            self.next = self.settled_at + self.wait(rate_limited_for);
        }
    }

    /// The computer woke or the clock was set: whatever failed before may
    /// work now.
    fn woke(&mut self) {
        self.failures = 0;
    }

    fn git_changed(&mut self, ids: Vec<String>, now: Instant) {
        self.git.extend(ids);
        self.git_first.get_or_insert(now);
        self.git_last = Some(now);
    }

    fn git_due(&self) -> Option<Instant> {
        Some((self.git_last? + GIT_QUIET).min(self.git_first? + GIT_MAX_DELAY))
    }

    /// The repositories whose checkouts changed, once the changes settled.
    fn take_git(&mut self, now: Instant) -> Vec<String> {
        if self.git_due().is_none_or(|due| due > now) {
            return Vec::new();
        }
        (self.git_first, self.git_last) = (None, None);
        std::mem::take(&mut self.git).into_iter().collect()
    }

    /// Whether the next timed full read is due.
    fn full_due(&self, now: Instant) -> bool {
        now >= self.next
    }

    /// When something next needs doing, if nothing comes in first.
    fn deadline(&self) -> Instant {
        self.git_due().map_or(self.next, |due| due.min(self.next))
    }

    fn pulse(&self) -> Pulse {
        Pulse { failures: self.failures, last_ok: self.last_ok, next_read: self.timed.then_some(self.next) }
    }
}

/// How long until GitHub's rate limit lets reads start again, if it stops
/// them now.
fn rate_limited_for(service: &ProductService, now: Timestamp) -> Option<Duration> {
    let until = service.next_allowed()?;
    Duration::try_from(until.duration_since(now)).ok()
}

/// A handle on the refresh threads.
pub(crate) struct Refresher {
    commands: Sender<Command>,
    service: Arc<ProductService>,
    queue: Arc<Queue>,
    pulse: Arc<Mutex<Pulse>>,
    /// The activity last told to the scheduler.
    activity: Mutex<Option<(bool, bool)>>,
    threads: Vec<JoinHandle<()>>,
}

impl Refresher {
    /// Starts reading `service` at once, calling `wake` whenever the state
    /// may have changed.
    pub(crate) fn spawn(service: Arc<ProductService>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let wake: Wake = Arc::new(wake);
        let (commands, inbox) = mpsc::channel();
        let queue = Arc::new(Queue::default());
        let pulse = Arc::new(Mutex::new(Pulse::default()));
        queue.update(|jobs| jobs.plan(false));

        if let Some(config) = service.config() {
            let commands = commands.clone();
            config.subscribe(move || {
                let _ = commands.send(Command::ConfigChanged);
            });
        }

        let mut threads = Vec::new();
        for _ in 0..PARALLEL_REPOSITORIES {
            let (service, queue, wake, commands) = (service.clone(), queue.clone(), wake.clone(), commands.clone());
            let spawned = thread::Builder::new()
                .name("signoff-read".into())
                .spawn(move || read(&service, &queue, &*wake, &commands));
            match spawned {
                Ok(thread) => threads.push(thread),
                Err(err) => log::error!("could not start a refresh thread: {err}"),
            }
        }
        let scheduler = {
            let (service, queue, pulse, commands) = (service.clone(), queue.clone(), pulse.clone(), commands.clone());
            thread::Builder::new()
                .name("signoff-refresh".into())
                .spawn(move || schedule(&service, &queue, &inbox, &commands, &*wake, &pulse))
        };
        match scheduler {
            Ok(thread) => threads.push(thread),
            Err(err) => log::error!("could not start the refresh thread: {err}"),
        }
        Self { commands, service, queue, pulse, activity: Mutex::new(None), threads }
    }

    /// Asks for everything to be read again, with the cached environment.
    pub(crate) fn refresh(&self) {
        let _ = self.commands.send(Command::Refresh { recheck: false });
    }

    /// Asks for everything to be read again, probing `gh` first: what the
    /// user's own Refresh does.
    pub(crate) fn recheck(&self) {
        let _ = self.commands.send(Command::Refresh { recheck: true });
    }

    /// Asks for one repository to be read again, ahead of other reads.
    pub(crate) fn refresh_repository(&self, id: &str) {
        let _ = self.commands.send(Command::Repository(id.to_owned()));
    }

    /// Tells the scheduler whether a window is open and whether a local run
    /// is in progress; it reads more often while either is. Cheap to call
    /// every frame: only a change is passed on.
    pub(crate) fn set_activity(&self, visible: bool, running: bool) {
        let mut last = self.activity.lock().unwrap_or_else(PoisonError::into_inner);
        if *last != Some((visible, running)) {
            *last = Some((visible, running));
            let _ = self.commands.send(Command::Activity { visible, running });
        }
    }

    /// Whether anything is being read or waits to be.
    pub(crate) fn is_refreshing(&self) -> bool {
        self.queue.lock().busy() || self.service.is_refreshing()
    }

    /// How reads are going.
    pub(crate) fn pulse(&self) -> Pulse {
        *self.pulse.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// When GitHub's rate limit lets reads start again, if it stops them.
    pub(crate) fn rate_limited_until(&self) -> Option<Timestamp> {
        self.service.next_allowed()
    }

    /// The state, if it changed since `seen`; updates `seen`.
    pub(crate) fn changed_since(&self, seen: &mut u64) -> Option<ProductState> {
        let revision = self.service.revision();
        if revision == *seen {
            return None;
        }
        *seen = revision;
        Some(self.service.cached())
    }

    /// Stops the threads once the reads in progress, if any, finish.
    pub(crate) fn stop(&mut self) {
        let _ = self.commands.send(Command::Quit);
        self.queue.update(|jobs| jobs.quit = true);
        // A read can wait on gh for a while; don't hold up quitting.
        self.threads.clear();
    }
}

impl Drop for Refresher {
    /// The threads hold senders of their own, so they would not notice the
    /// handle going.
    fn drop(&mut self) {
        self.stop();
    }
}

/// A reader: runs jobs until quitting, telling the scheduler how each read
/// went and when the queue runs empty.
fn read(service: &ProductService, queue: &Queue, wake: &dyn Fn(), commands: &Sender<Command>) {
    while let Some(job) = queue.wait() {
        match &job {
            Job::Plan { recheck } => {
                let ids = service.plan_refresh(*recheck);
                queue.update(|jobs| ids.into_iter().for_each(|id| jobs.read(id, false)));
            }
            Job::Read(id) => {
                let landed = service.refresh_repository(id);
                // A rate-limited read never reached GitHub.
                let verdict = (landed && service.next_allowed().is_none()).then(|| service.read_failed(id));
                let _ = commands.send(Command::Read(verdict));
            }
        }
        let mut idle = false;
        queue.update(|jobs| {
            jobs.finish(&job);
            idle = !jobs.busy();
        });
        if idle {
            let _ = commands.send(Command::Idle);
        }
        wake();
    }
}

/// Watches the checkouts of the repositories that are read.
fn watch(watcher: &mut Option<GitWatcher>, service: &ProductService) {
    let (Some(watcher), Some(config)) = (watcher, service.config()) else { return };
    let repositories: Vec<(String, PathBuf)> = config
        .snapshot()
        .repositories
        .into_iter()
        .filter(|repo| repo.enabled)
        .map(|repo| (repo.id, repo.path))
        .collect();
    watcher.watch(repositories);
}

/// The scheduler: turns commands, the cadence and the watchers into jobs.
fn schedule(
    service: &ProductService,
    queue: &Queue,
    inbox: &mpsc::Receiver<Command>,
    commands: &Sender<Command>,
    wake: &dyn Fn(),
    pulse: &Mutex<Pulse>,
) {
    let mut plan = Schedule::new(Instant::now(), !service.is_demo());
    let mut watcher = service.config().map(|_| {
        let commands = commands.clone();
        GitWatcher::new(move |ids| {
            let _ = commands.send(Command::GitChanged(ids));
        })
    });
    watch(&mut watcher, service);
    loop {
        let (asked, asked_wall) = (Instant::now(), Timestamp::now());
        let expected = plan.deadline().saturating_duration_since(asked).min(TICK);
        let received = inbox.recv_timeout(expected);
        let (now, wall) = (Instant::now(), Timestamp::now());
        let rate = rate_limited_for(service, wall);
        if cadence::clock_jumped(expected, now - asked, wall.duration_since(asked_wall)) {
            log::info!("the clock jumped (asleep?); reading everything again");
            plan.woke();
            queue.update(|jobs| jobs.plan(true));
            plan.started_full(now, rate);
        }
        match received {
            Ok(Command::Refresh { recheck }) => {
                queue.update(|jobs| jobs.plan(recheck));
                plan.started_full(now, rate);
            }
            Ok(Command::Repository(id)) => queue.update(|jobs| jobs.read(id, true)),
            Ok(Command::ConfigChanged) => {
                let ids = service.sync_config();
                queue.update(|jobs| ids.into_iter().for_each(|id| jobs.read(id, true)));
                watch(&mut watcher, service);
                wake();
            }
            Ok(Command::Activity { visible, running }) => plan.set_activity(visible, running, rate),
            Ok(Command::GitChanged(ids)) => plan.git_changed(ids, now),
            Ok(Command::Read(failed)) => plan.record(failed),
            Ok(Command::Idle) => {
                plan.settle(now, wall, &service.cached(), rate);
                // The readers woke the UI before this settled.
                wake();
            }
            Ok(Command::Quit) | Err(RecvTimeoutError::Disconnected) => {
                queue.update(|jobs| jobs.quit = true);
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        let changed = plan.take_git(now);
        if !changed.is_empty() {
            queue.update(|jobs| changed.into_iter().for_each(|id| jobs.read(id, true)));
        }
        if plan.full_due(now) {
            queue.update(|jobs| jobs.plan(false));
            plan.started_full(now, rate);
        }
        *pulse.lock().unwrap_or_else(PoisonError::into_inner) = plan.pulse();
    }
}

#[cfg(test)]
mod tests {
    use signoff_core::{Check, PullRequestState};

    use super::*;

    fn drain(jobs: &mut Jobs) -> Vec<Job> {
        let mut taken = Vec::new();
        while let Some(job) = jobs.take() {
            taken.push(job);
        }
        taken
    }

    #[test]
    fn duplicate_reads_wait_once_and_urgent_ones_go_first() {
        let mut jobs = Jobs::default();
        jobs.read("a".into(), false);
        jobs.read("b".into(), false);
        jobs.read("a".into(), false);
        jobs.read("b".into(), true);
        assert_eq!(drain(&mut jobs), [Job::Read("b".into()), Job::Read("a".into())]);
        assert_eq!(jobs.running, 2);
    }

    #[test]
    fn plans_merge_and_never_overlap() {
        let mut jobs = Jobs::default();
        jobs.plan(false);
        jobs.plan(true);
        let plan = jobs.take().expect("plan");
        assert_eq!(plan, Job::Plan { recheck: true });

        jobs.plan(false);
        jobs.read("a".into(), false);
        assert_eq!(jobs.take(), Some(Job::Read("a".into())), "reads pass a plan that must wait");
        assert_eq!(jobs.take(), None);
        jobs.finish(&plan);
        assert_eq!(jobs.take(), Some(Job::Plan { recheck: false }));
        assert!(jobs.busy());
    }

    // --- The schedule, on a fake clock -----------------------------------

    const fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn wall(second: i64) -> Timestamp {
        Timestamp::from_second(1_800_000_000 + second).expect("timestamp")
    }

    /// A state with one open pull request, its check pending or not.
    fn open(pending: bool) -> ProductState {
        let state = if pending { CheckState::Pending } else { CheckState::Absent };
        let check = Check { name: "tests".into(), state, ..Check::default() };
        ProductState {
            pull_requests: vec![PullRequestState { checks: vec![check], ..Default::default() }],
            ..Default::default()
        }
    }

    /// A schedule that started at `t0` and settled its first full read.
    fn settled(t0: Instant, state: &ProductState) -> Schedule {
        let mut s = Schedule::new(t0, true);
        s.record(Some(false));
        s.settle(t0, wall(0), state, None);
        s
    }

    #[test]
    fn full_reads_follow_the_cadence_from_when_the_last_one_settled() {
        let t0 = Instant::now();
        let mut s = settled(t0, &open(false));
        assert_eq!(s.pulse(), Pulse { failures: 0, last_ok: Some(wall(0)), next_read: Some(t0 + cadence::BACKGROUND) });
        assert!(!s.full_due(t0 + secs(59)));
        assert!(s.full_due(t0 + secs(60)));

        s.started_full(t0 + secs(60), None);
        s.settle(t0 + secs(63), wall(63), &ProductState::default(), None);
        assert_eq!(s.deadline(), t0 + secs(63) + cadence::QUIET, "nothing open: rarely");

        s.started_full(t0 + secs(400), None);
        s.settle(t0 + secs(401), wall(401), &open(true), None);
        assert_eq!(s.deadline(), t0 + secs(401) + cadence::LIVE, "a pending check: often");
    }

    #[test]
    fn opening_the_window_or_running_a_check_reads_sooner_and_closing_it_later() {
        let t0 = Instant::now();
        let mut s = settled(t0, &open(false));
        s.set_activity(true, false, None);
        assert_eq!(s.deadline(), t0 + cadence::LIVE);
        s.set_activity(false, false, None);
        assert_eq!(s.deadline(), t0 + cadence::BACKGROUND);
        s.set_activity(false, true, None);
        assert_eq!(s.deadline(), t0 + cadence::LIVE);

        s.started_full(t0 + secs(10), None);
        s.set_activity(false, false, None);
        assert_eq!(s.deadline(), t0 + secs(10) + cadence::LIVE, "a read on its way is not retimed");
    }

    #[test]
    fn failed_rounds_back_off_and_a_success_or_a_wake_resets_them() {
        let t0 = Instant::now();
        let mut s = settled(t0, &open(false));
        s.set_activity(true, false, None);
        for (round, wait) in [(1, 20), (2, 40), (3, 80)] {
            s.started_full(t0, None);
            s.record(Some(true));
            s.record(Some(true));
            s.settle(t0, wall(0), &open(false), None);
            assert_eq!((s.failures, s.deadline()), (round, t0 + secs(wait)));
        }
        assert_eq!(s.pulse().last_ok, Some(wall(0)), "the last success stays");

        s.record(Some(true));
        s.record(Some(false));
        s.settle(t0, wall(5), &open(false), None);
        assert_eq!(s.failures, 0, "one repository failing among others is not the connection");
        assert_eq!(s.pulse().last_ok, Some(wall(5)));

        s.record(Some(true));
        s.settle(t0, wall(6), &open(false), None);
        s.woke();
        assert_eq!(s.failures, 0);

        s.record(None);
        s.settle(t0, wall(7), &open(false), None);
        assert_eq!(s.pulse().last_ok, Some(wall(7)), "a round with nothing to read succeeded");
    }

    #[test]
    fn the_rate_limit_holds_reads_back() {
        let t0 = Instant::now();
        let mut s = Schedule::new(t0, true);
        s.settle(t0, wall(0), &open(true), Some(secs(600)));
        assert_eq!(s.deadline(), t0 + secs(600));
        s.set_activity(true, false, Some(secs(590)));
        assert_eq!(s.deadline(), t0 + secs(590));
    }

    #[test]
    fn reading_one_repository_does_not_put_off_reading_them_all() {
        let t0 = Instant::now();
        let mut s = settled(t0, &open(false));
        s.record(Some(false));
        s.settle(t0 + secs(50), wall(50), &open(false), None);
        assert_eq!(s.deadline(), t0 + cadence::BACKGROUND);
    }

    #[test]
    fn checkout_changes_are_read_once_they_stop() {
        let t0 = Instant::now();
        let mut s = settled(t0, &open(false));
        s.git_changed(vec!["a".into()], t0);
        assert_eq!(s.deadline(), t0 + GIT_QUIET);
        s.git_changed(vec!["b".into(), "a".into()], t0 + Duration::from_millis(200));
        assert!(s.take_git(t0 + Duration::from_millis(300)).is_empty(), "still changing");
        assert_eq!(s.take_git(t0 + Duration::from_millis(500)), ["a", "b"]);
        assert!(s.take_git(t0 + secs(1)).is_empty(), "taken once");
        assert_eq!(s.deadline(), t0 + cadence::BACKGROUND);

        // A rebase that keeps changing them is read every couple of seconds.
        let t1 = t0 + secs(5);
        for step in 0..10 {
            s.git_changed(vec!["a".into()], t1 + Duration::from_millis(step * 250));
        }
        assert_eq!(s.deadline(), t1 + GIT_MAX_DELAY);
        assert_eq!(s.take_git(t1 + GIT_MAX_DELAY), ["a"]);
    }

    #[test]
    fn demo_data_is_never_read_on_a_timer() {
        let t0 = Instant::now();
        let mut s = Schedule::new(t0, false);
        s.settle(t0, wall(0), &open(true), None);
        assert_eq!(s.deadline(), t0 + UNTIMED);
        assert_eq!(s.pulse().next_read, None);
        assert_eq!(s.pulse().last_ok, Some(wall(0)));
    }

    #[test]
    fn a_demo_refresher_serves_the_demo_state_and_goes_live() {
        let service = Arc::new(ProductService::demo());
        let (woken, waiting) = mpsc::channel();
        let woken = Mutex::new(woken);
        let mut refresher = Refresher::spawn(service, move || {
            let _ = woken.lock().map(|w| w.send(()));
        });
        let mut seen = 0;
        let deadline = Instant::now() + Duration::from_secs(10);
        let state = loop {
            let _ = waiting.recv_timeout(Duration::from_millis(50));
            if let Some(state) = refresher.changed_since(&mut seen) {
                break state;
            }
            assert!(Instant::now() < deadline, "timed out");
        };
        assert!(state.demo && !state.pull_requests.is_empty());
        assert!(refresher.changed_since(&mut seen).is_none(), "copied only when it changed");
        while refresher.pulse().last_ok.is_none() {
            let _ = waiting.recv_timeout(Duration::from_millis(50));
            assert!(Instant::now() < deadline, "timed out settling");
        }
        assert_eq!(refresher.pulse().failures, 0);
        refresher.stop();
    }
}
