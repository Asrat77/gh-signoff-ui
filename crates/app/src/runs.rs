//! Verify-and-sign-off runs, each on its own thread.
//!
//! [`Verifier::verify`] blocks for as long as the user's command runs, so
//! every run gets a thread. Its output is collected here for the log view,
//! and the UI is woken as lines arrive.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use signoff_core::{ActivityEvent, Outcome, Phase, Verifier, VerifyEvent, VerifyRequest};
use signoff_ui::Status;
use signoff_ui::widgets::LogLine;

use crate::notifier::RunFinished;

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
    /// Repositories a run signed off on since the app last looked, so it
    /// reads them again.
    signed_off: Arc<Mutex<Vec<String>>>,
    /// Runs that ended since the app last looked, for notifications.
    finished: Arc<Mutex<Vec<RunFinished>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Runs {
    pub(crate) fn new(verifier: Arc<Verifier>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            verifier,
            slots: Arc::default(),
            signed_off: Arc::default(),
            finished: Arc::default(),
            wake: Arc::new(wake),
        }
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
        let finished = self.finished.clone();
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
                signed_off.lock().unwrap_or_else(PoisonError::into_inner).push(request.repo_id.clone());
            }
            let result = result.map_err(|err| err.to_string());
            finished
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(RunFinished { request: request.clone(), result: result.clone() });
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

    /// Whether any run is in progress.
    pub(crate) fn any_running(&self) -> bool {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|s| s.view.running())
    }

    /// The repositories a run signed off on since the last call.
    pub(crate) fn take_signed_off(&self) -> Vec<String> {
        let mut repos = std::mem::take(&mut *self.signed_off.lock().unwrap_or_else(PoisonError::into_inner));
        repos.sort();
        repos.dedup();
        repos
    }

    /// The runs that ended since the last call.
    pub(crate) fn take_finished(&self) -> Vec<RunFinished> {
        std::mem::take(&mut *self.finished.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Stops every run, for quitting.
    pub(crate) fn stop_all(&self) {
        for slot in self.slots.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            slot.cancel.store(true, Ordering::Release);
        }
    }
}

/// A past run's output, as history kept it, for the log view, ending with
/// how it went. Empty when history kept no output for it.
pub(crate) fn past_log(event: &ActivityEvent) -> Vec<LogLine> {
    if event.output.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::with_capacity(event.output.len() + 2);
    if event.output_dropped > 0 {
        lines.push(LogLine::Step(format!("… {} earlier lines", event.output_dropped)));
    }
    lines.extend(event.output.iter().cloned().map(LogLine::Output));
    let (end, _) = past_result(event);
    lines.push(if event.exit_code == 0 { LogLine::Success(end) } else { LogLine::Failure(end) });
    lines
}

/// "exit 0 in 6.2s" for a past run, and how it reads.
pub(crate) fn past_result(event: &ActivityEvent) -> (String, Status) {
    let status = if event.exit_code == 0 { Status::Ready } else { Status::Failing };
    let text = if event.duration_millis > 0 {
        format!("exit {} in {:.1}s", event.exit_code, event.duration_millis as f64 / 1000.0)
    } else {
        format!("exit {}", event.exit_code)
    };
    (text, status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_past_run_reads_back_with_how_it_ended() {
        let event = ActivityEvent {
            output: vec!["$ make test".into(), "1 failed".into()],
            output_dropped: 3,
            exit_code: 2,
            duration_millis: 1500,
            ..ActivityEvent::default()
        };
        assert_eq!(
            past_log(&event),
            [
                LogLine::Step("… 3 earlier lines".into()),
                LogLine::Output("$ make test".into()),
                LogLine::Output("1 failed".into()),
                LogLine::Failure("exit 2 in 1.5s".into()),
            ]
        );
        assert_eq!(past_result(&event).1, Status::Failing);
    }

    #[test]
    fn an_entry_without_output_has_no_log() {
        assert!(past_log(&ActivityEvent::default()).is_empty());
    }
}
