//! The background threads that keep the state current.
//!
//! Reading GitHub blocks on `gh`, so none of it runs on the UI thread. Two
//! kinds of thread share the work:
//!
//! - **The scheduler** decides when to read: everything on start, every
//!   `refreshSeconds` and when asked; one repository when the config
//!   changes it or a signoff lands on it. Config changes are also applied
//!   to the state at once, before anything is read.
//! - **Readers**, a few of them, take jobs from a queue that drops
//!   duplicates and puts single-repository reads first. A full refresh is a
//!   plan job (sync the config, probe the environment if needed) that
//!   queues one read per repository, so repositories land one by one.
//!
//! [`ProductService`] patches each read into the state and moves its
//! revision; the readers wake the UI after every job, and the UI copies the
//! state when the revision moved. Smarter timing (triggers, an adaptive
//! cadence, git watchers) replaces the scheduler loop alone; cheaper reads
//! change what a read job costs, not how jobs flow.

use std::collections::VecDeque;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use signoff_core::product::PARALLEL_REPOSITORIES;
use signoff_core::{ProductService, ProductState};

type Wake = Arc<dyn Fn() + Send + Sync>;

/// What the scheduler is asked to do.
enum Command {
    /// Read everything; `recheck` probes the environment again too.
    Refresh {
        recheck: bool,
    },
    /// Read one repository soon.
    Repository(String),
    /// The config changed: patch the state, then read what needs it.
    ConfigChanged,
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

/// A handle on the refresh threads.
pub(crate) struct Refresher {
    commands: Sender<Command>,
    service: Arc<ProductService>,
    queue: Arc<Queue>,
    threads: Vec<JoinHandle<()>>,
}

impl Refresher {
    /// Starts reading `service` at once, calling `wake` whenever the state
    /// may have changed.
    pub(crate) fn spawn(service: Arc<ProductService>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let wake: Wake = Arc::new(wake);
        let (commands, inbox) = mpsc::channel();
        let queue = Arc::new(Queue::default());
        queue.update(|jobs| jobs.plan(false));

        if let Some(config) = service.config() {
            let commands = commands.clone();
            config.subscribe(move || {
                let _ = commands.send(Command::ConfigChanged);
            });
        }

        let mut threads = Vec::new();
        for _ in 0..PARALLEL_REPOSITORIES {
            let (service, queue, wake) = (service.clone(), queue.clone(), wake.clone());
            match thread::Builder::new().name("signoff-read".into()).spawn(move || read(&service, &queue, &*wake)) {
                Ok(thread) => threads.push(thread),
                Err(err) => log::error!("could not start a refresh thread: {err}"),
            }
        }
        let scheduler = {
            let (service, queue) = (service.clone(), queue.clone());
            thread::Builder::new()
                .name("signoff-refresh".into())
                .spawn(move || schedule(&service, &queue, &inbox, &*wake))
        };
        match scheduler {
            Ok(thread) => threads.push(thread),
            Err(err) => log::error!("could not start the refresh thread: {err}"),
        }
        Self { commands, service, queue, threads }
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

    /// Whether anything is being read or waits to be.
    pub(crate) fn is_refreshing(&self) -> bool {
        self.queue.lock().busy() || self.service.is_refreshing()
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

/// A reader: runs jobs until quitting.
fn read(service: &ProductService, queue: &Queue, wake: &dyn Fn()) {
    while let Some(job) = queue.wait() {
        match &job {
            Job::Plan { recheck } => {
                let ids = service.plan_refresh(*recheck);
                queue.update(|jobs| ids.into_iter().for_each(|id| jobs.read(id, false)));
            }
            Job::Read(id) => {
                service.refresh_repository(id);
            }
        }
        queue.update(|jobs| jobs.finish(&job));
        wake();
    }
}

/// The scheduler: turns commands and the interval into jobs.
fn schedule(service: &ProductService, queue: &Queue, inbox: &mpsc::Receiver<Command>, wake: &dyn Fn()) {
    let mut next = Instant::now() + interval(service);
    loop {
        match inbox.recv_timeout(next.saturating_duration_since(Instant::now())) {
            Ok(Command::Refresh { recheck }) => {
                queue.update(|jobs| jobs.plan(recheck));
                next = Instant::now() + interval(service);
            }
            Err(RecvTimeoutError::Timeout) => {
                queue.update(|jobs| jobs.plan(false));
                next = Instant::now() + interval(service);
            }
            Ok(Command::Repository(id)) => queue.update(|jobs| jobs.read(id, true)),
            Ok(Command::ConfigChanged) => {
                let ids = service.sync_config();
                queue.update(|jobs| ids.into_iter().for_each(|id| jobs.read(id, true)));
                wake();
                // The interval may have just been shortened.
                next = next.min(Instant::now() + interval(service));
            }
            Ok(Command::Quit) | Err(RecvTimeoutError::Disconnected) => {
                queue.update(|jobs| jobs.quit = true);
                break;
            }
        }
    }
}

/// How long between full refreshes.
fn interval(service: &ProductService) -> Duration {
    match service.config() {
        Some(config) => Duration::from_secs(u64::from(config.snapshot().refresh_seconds)),
        // Demo data never changes on its own.
        None => Duration::from_secs(3600),
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn a_demo_refresher_serves_the_demo_state() {
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
        refresher.stop();
    }
}
