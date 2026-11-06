//! Installing the `gh signoff` extension, on its own thread.
//!
//! `gh extension install` downloads from GitHub, so it never runs on the UI
//! thread. The popover and Settings read whether it is running and what went
//! wrong last time; the app takes each result once, to toast it and to check
//! the environment again.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use signoff_core::Gh;

/// How long the demo's simulated install takes.
const DEMO_DELAY: Duration = Duration::from_millis(600);

/// Runs the install and remembers how it went.
pub(crate) struct Installer {
    gh: Gh,
    /// Simulate success instead of running anything.
    demo: bool,
    running: Arc<AtomicBool>,
    /// Why the last install failed, until the next one starts.
    failure: Arc<Mutex<Option<String>>>,
    finished: Arc<Mutex<Vec<Result<(), String>>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Installer {
    pub(crate) fn new(gh: Gh, demo: bool, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            gh,
            demo,
            running: Arc::default(),
            failure: Arc::default(),
            finished: Arc::default(),
            wake: Arc::new(wake),
        }
    }

    /// Starts the install unless one is already running.
    pub(crate) fn start(&self) {
        if self.running.swap(true, Ordering::AcqRel) {
            return;
        }
        *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let (gh, demo) = (self.gh.clone(), self.demo);
        let (running, failure, finished, wake) =
            (self.running.clone(), self.failure.clone(), self.finished.clone(), self.wake.clone());
        let spawned = thread::Builder::new().name("signoff-install".into()).spawn(move || {
            let result = if demo {
                thread::sleep(DEMO_DELAY);
                Ok(())
            } else {
                gh.install_signoff_extension()
            };
            if let Err(message) = &result {
                *failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(message.clone());
            }
            finished.lock().unwrap_or_else(PoisonError::into_inner).push(result);
            running.store(false, Ordering::Release);
            wake();
        });
        if let Err(err) = spawned {
            self.running.store(false, Ordering::Release);
            log::error!("could not start an install thread: {err}");
        }
    }

    /// Whether the install is running.
    pub(crate) fn running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Why the last install failed, if it did.
    pub(crate) fn failure(&self) -> Option<String> {
        self.failure.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The installs that ended since the last call.
    pub(crate) fn take_finished(&self) -> Vec<Result<(), String>> {
        std::mem::take(&mut *self.finished.lock().unwrap_or_else(PoisonError::into_inner))
    }
}
