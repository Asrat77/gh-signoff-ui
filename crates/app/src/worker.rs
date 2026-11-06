//! The background thread that refreshes the state.
//!
//! [`ProductService::refresh`] blocks on `gh` for as long as GitHub takes,
//! so it never runs on the UI thread. The worker refreshes on start, then
//! every `refreshSeconds` or when asked, and wakes the UI after each one.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use signoff_core::{ProductService, ProductState};

enum Command {
    Refresh,
    Quit,
}

/// What the worker shares with the UI.
#[derive(Default)]
struct Shared {
    state: Mutex<ProductState>,
    /// Bumped after every refresh, so the UI copies the state only when it
    /// changed.
    generation: AtomicU64,
    refreshing: AtomicBool,
}

/// A handle on the refresh thread.
pub(crate) struct Refresher {
    commands: Sender<Command>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Refresher {
    /// Starts refreshing `service` at once, calling `wake` after each
    /// refresh.
    pub(crate) fn spawn(service: Arc<ProductService>, wake: impl Fn() + Send + 'static) -> Self {
        let (commands, inbox) = mpsc::channel();
        let shared = Arc::new(Shared::default());
        let worker = shared.clone();
        let thread = thread::Builder::new()
            .name("signoff-refresh".into())
            .spawn(move || {
                loop {
                    worker.refreshing.store(true, Ordering::Release);
                    wake();
                    let state = service.refresh();
                    *worker.state.lock().unwrap_or_else(PoisonError::into_inner) = state;
                    worker.refreshing.store(false, Ordering::Release);
                    worker.generation.fetch_add(1, Ordering::AcqRel);
                    wake();

                    let interval = match service.config() {
                        Some(config) => Duration::from_secs(u64::from(config.snapshot().refresh_seconds)),
                        // Demo data never changes on its own.
                        None => Duration::from_secs(3600),
                    };
                    match inbox.recv_timeout(interval) {
                        Ok(Command::Refresh) | Err(RecvTimeoutError::Timeout) => {}
                        Ok(Command::Quit) | Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .ok();
        if thread.is_none() {
            log::error!("could not start the refresh thread");
        }
        Self { commands, shared, thread }
    }

    /// Asks for a refresh now. One already running finishes first.
    pub(crate) fn refresh(&self) {
        let _ = self.commands.send(Command::Refresh);
    }

    /// Whether a refresh is running.
    pub(crate) fn is_refreshing(&self) -> bool {
        self.shared.refreshing.load(Ordering::Acquire)
    }

    /// The state, if it changed since `seen`; updates `seen`.
    pub(crate) fn changed_since(&self, seen: &mut u64) -> Option<ProductState> {
        let generation = self.shared.generation.load(Ordering::Acquire);
        if generation == *seen {
            return None;
        }
        *seen = generation;
        Some(self.shared.state.lock().unwrap_or_else(PoisonError::into_inner).clone())
    }

    /// Stops the thread once the refresh in progress, if any, finishes.
    pub(crate) fn stop(&mut self) {
        let _ = self.commands.send(Command::Quit);
        // A refresh can wait on gh for a while; don't hold up quitting.
        drop(self.thread.take());
    }
}
