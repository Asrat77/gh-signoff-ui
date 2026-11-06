//! Verify-and-sign-off runs, each on its own thread.
//!
//! [`Verifier::verify`] blocks for as long as the user's command runs, so
//! every run gets a thread. Its output is collected here for the log view,
//! and the UI is woken as lines arrive.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use signoff_core::{Outcome, Phase, Verifier, VerifyEvent, VerifyRequest};

/// Lines kept per run; older ones scroll away.
const MAX_LINES: usize = 4000;

/// One run, as the UI shows it.
#[derive(Clone, Debug)]
pub(crate) struct RunView {
    pub(crate) request: VerifyRequest,
    pub(crate) phase: Phase,
    pub(crate) lines: VecDeque<String>,
    /// Lines dropped from the front to stay under [`MAX_LINES`].
    pub(crate) dropped: usize,
    /// `None` while running.
    pub(crate) result: Option<Result<Outcome, String>>,
}

impl RunView {
    pub(crate) fn running(&self) -> bool {
        self.result.is_none()
    }

    fn push(&mut self, line: String) {
        if self.lines.len() == MAX_LINES {
            self.lines.pop_front();
            self.dropped += 1;
        }
        self.lines.push_back(line);
    }
}

struct Slot {
    view: RunView,
    cancel: Arc<AtomicBool>,
}

/// The runs started this session, newest last.
pub(crate) struct Runs {
    verifier: Arc<Verifier>,
    slots: Arc<Mutex<Vec<Slot>>>,
    /// Bumped when a run signs off, so the app knows to refresh.
    signed_off: Arc<AtomicU64>,
    seen_signed_off: u64,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Runs {
    pub(crate) fn new(verifier: Arc<Verifier>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self { verifier, slots: Arc::default(), signed_off: Arc::default(), seen_signed_off: 0, wake: Arc::new(wake) }
    }

    /// Starts verifying `request` unless it is already running.
    pub(crate) fn start(&self, request: VerifyRequest) {
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
            if slots.iter().any(|s| s.view.request == request && s.view.running()) {
                return;
            }
            // Only the latest run of a check is kept.
            slots.retain(|s| s.view.request != request);
            let view = RunView {
                request: request.clone(),
                phase: Phase::Run,
                lines: VecDeque::new(),
                dropped: 0,
                result: None,
            };
            slots.push(Slot { view, cancel: cancel.clone() });
        }

        let verifier = self.verifier.clone();
        let slots = self.slots.clone();
        let signed_off = self.signed_off.clone();
        let wake = self.wake.clone();
        let spawned = thread::Builder::new().name("signoff-verify".into()).spawn(move || {
            let update = |change: &mut dyn FnMut(&mut RunView)| {
                let mut slots = slots.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(slot) = slots.iter_mut().rev().find(|s| s.view.request == request) {
                    change(&mut slot.view);
                }
                drop(slots);
                wake();
            };
            let result = verifier.verify(&request, &cancel, &mut |event| match event {
                VerifyEvent::Phase(phase) => update(&mut |view| view.phase = phase),
                VerifyEvent::Line(line) => update(&mut |view| view.push(line.clone())),
            });
            if result.as_ref().is_ok_and(Outcome::signed_off) {
                signed_off.fetch_add(1, Ordering::AcqRel);
            }
            let result = result.map_err(|err| err.to_string());
            update(&mut |view| view.result = Some(result.clone()));
        });
        if let Err(err) = spawned {
            log::error!("could not start a verify thread: {err}");
        }
    }

    /// Asks a running `request` to stop.
    pub(crate) fn stop(&self, request: &VerifyRequest) {
        let slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = slots.iter().rev().find(|s| &s.view.request == request) {
            slot.cancel.store(true, Ordering::Release);
        }
    }

    /// Forgets a finished run.
    pub(crate) fn dismiss(&self, request: &VerifyRequest) {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|s| &s.view.request != request || s.view.running());
    }

    /// The latest run of `request`, if any.
    pub(crate) fn view(&self, request: &VerifyRequest) -> Option<RunView> {
        let slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        slots.iter().rev().find(|s| &s.view.request == request).map(|s| s.view.clone())
    }

    /// Whether a run signed off since the last call.
    pub(crate) fn take_signed_off(&mut self) -> bool {
        let now = self.signed_off.load(Ordering::Acquire);
        let changed = now != self.seen_signed_off;
        self.seen_signed_off = now;
        changed
    }

    /// Stops every run, for quitting.
    pub(crate) fn stop_all(&self) {
        for slot in self.slots.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            slot.cancel.store(true, Ordering::Release);
        }
    }
}
