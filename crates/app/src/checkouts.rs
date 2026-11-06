//! "Check out this pull request" jobs, each on its own thread.
//!
//! `gh pr checkout` can take a while on a big repository, so it never runs
//! on the UI thread. The pages read each job's [`Checkout`] state; finished
//! jobs are handed to the app once, which toasts them and reads the
//! repository again so the page sees the new head.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use signoff_core::Gh;

/// How long the demo's simulated checkout takes.
const DEMO_DELAY: Duration = Duration::from_millis(600);

/// How long a finished checkout waits for the refresh that confirms it
/// before the page offers the button again.
const CONFIRM_WITHIN: Duration = Duration::from_secs(15);

/// Where one pull request's checkout stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Checkout {
    /// `gh pr checkout` is running.
    Running,
    /// It finished; the repository is being read to confirm.
    Done,
    /// It was refused or failed, with a message for the user.
    Failed(String),
}

/// A job that ended, for the app to announce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Finished {
    pub(crate) repo_id: String,
    pub(crate) number: u64,
    pub(crate) result: Result<(), String>,
}

struct Slot {
    repo_id: String,
    number: u64,
    state: Checkout,
    at: Instant,
}

/// The checkouts started this session.
pub(crate) struct Checkouts {
    gh: Gh,
    /// Simulate success instead of running anything.
    demo: bool,
    slots: Arc<Mutex<Vec<Slot>>>,
    finished: Arc<Mutex<Vec<Finished>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Checkouts {
    pub(crate) fn new(gh: Gh, demo: bool, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self { gh, demo, slots: Arc::default(), finished: Arc::default(), wake: Arc::new(wake) }
    }

    /// Starts checking out pull request `number` in `dir`, unless that is
    /// already running.
    pub(crate) fn start(&self, repo_id: &str, number: u64, dir: PathBuf) {
        {
            let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
            if slots.iter().any(|s| s.repo_id == repo_id && s.number == number && s.state == Checkout::Running) {
                return;
            }
            slots.retain(|s| !(s.repo_id == repo_id && s.number == number));
            slots.push(Slot { repo_id: repo_id.to_owned(), number, state: Checkout::Running, at: Instant::now() });
        }
        let (gh, demo) = (self.gh.clone(), self.demo);
        let (slots, finished, wake) = (self.slots.clone(), self.finished.clone(), self.wake.clone());
        let repo_id = repo_id.to_owned();
        let spawned = thread::Builder::new().name("signoff-checkout".into()).spawn(move || {
            let result = if demo {
                thread::sleep(DEMO_DELAY);
                Ok(())
            } else {
                gh.check_out_pull_request(&dir, number)
            };
            {
                let mut slots = slots.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(slot) = slots.iter_mut().find(|s| s.repo_id == repo_id && s.number == number) {
                    slot.state = match &result {
                        Ok(()) => Checkout::Done,
                        Err(message) => Checkout::Failed(message.clone()),
                    };
                    slot.at = Instant::now();
                }
            }
            finished.lock().unwrap_or_else(PoisonError::into_inner).push(Finished { repo_id, number, result });
            wake();
        });
        if let Err(err) = spawned {
            log::error!("could not start a checkout thread: {err}");
        }
    }

    /// The latest checkout of this pull request, if any. A finished one
    /// that the refresh never confirmed is forgotten after a while.
    pub(crate) fn view(&self, repo_id: &str, number: u64) -> Option<Checkout> {
        let slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = slots.iter().find(|s| s.repo_id == repo_id && s.number == number)?;
        match slot.state {
            // The demo has nothing to read again: it stays checked out.
            Checkout::Done if !self.demo && slot.at.elapsed() > CONFIRM_WITHIN => None,
            _ => Some(slot.state.clone()),
        }
    }

    /// Whether the demo pretends this pull request is checked out.
    pub(crate) fn simulated(&self, repo_id: &str, number: u64) -> bool {
        self.demo && self.view(repo_id, number) == Some(Checkout::Done)
    }

    /// The jobs that ended since the last call.
    pub(crate) fn take_finished(&self) -> Vec<Finished> {
        std::mem::take(&mut *self.finished.lock().unwrap_or_else(PoisonError::into_inner))
    }
}
