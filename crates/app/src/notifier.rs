//! Desktop notifications: which ones to show, and showing them.
//!
//! What counts is decided in core ([`signoff_core::transitions`]) and here
//! for a finished local run. Delivery goes through the operating system's
//! notification service on its own thread, since talking to it can wait.

use std::thread;

use signoff_core::{CANCELLED, Outcome, ProductState, Transition, VerifyRequest};

/// A local run that ended, for the app to announce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunFinished {
    pub(crate) request: VerifyRequest,
    pub(crate) result: Result<Outcome, String>,
}

/// One notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Notice {
    pub(crate) title: String,
    pub(crate) body: String,
}

impl Notice {
    fn of_transition(transition: &Transition) -> Self {
        Self { title: transition.headline(), body: transition.detail() }
    }

    /// What a finished run says; a run the person stopped says nothing.
    fn of_run(run: &RunFinished, state: &ProductState) -> Option<Self> {
        let number = run.request.pr_number;
        let check = &run.request.check;
        let place = state
            .pull_request(&run.request.repo_id, number)
            .map(|pr| format!("{} in {}", pr.pr.title, pr.repo))
            .unwrap_or_default();
        match &run.result {
            Ok(outcome) if outcome.code == CANCELLED => None,
            Ok(outcome) if outcome.signed_off() => {
                Some(Self { title: format!("Signed off {check} on #{number}"), body: place })
            }
            Ok(outcome) => {
                let why = outcome.message.clone().unwrap_or_else(|| format!("exited with {}", outcome.code));
                Some(Self { title: format!("{check} failed on #{number}"), body: join(&why, &place) })
            }
            Err(message) => Some(Self { title: format!("Could not run {check} on #{number}"), body: message.clone() }),
        }
    }
}

fn join(first: &str, second: &str) -> String {
    if second.is_empty() { first.to_owned() } else { format!("{first}\n{second}") }
}

/// The notifications to show now. Nothing while the setting is off or the
/// popover is in view: it already shows the change.
pub(crate) fn notices(
    enabled: bool,
    hidden: bool,
    transitions: &[Transition],
    runs: &[RunFinished],
    state: &ProductState,
) -> Vec<Notice> {
    if !enabled || !hidden {
        return Vec::new();
    }
    let runs = runs.iter().filter_map(|run| Notice::of_run(run, state));
    transitions.iter().map(Notice::of_transition).chain(runs).collect()
}

/// Shows `notices` on a background thread. A system without a notification
/// service only logs.
pub(crate) fn deliver(notices: Vec<Notice>) {
    if notices.is_empty() {
        return;
    }
    let spawned = thread::Builder::new().name("signoff-notify".into()).spawn(move || {
        for notice in notices {
            show(&notice);
        }
    });
    if let Err(err) = spawned {
        log::error!("could not start a notification thread: {err}");
    }
}

fn show(notice: &Notice) {
    let shown = notify_rust::Notification::new().appname("Signoff").summary(&notice.title).body(&notice.body).show();
    if let Err(err) = shown {
        log::warn!("could not show a notification: {err}");
    }
}

#[cfg(test)]
mod tests {
    use signoff_core::{Change, Phase};

    use super::*;

    fn transition(change: Change) -> Transition {
        Transition { repo_id: "r".into(), repo: "acme/widgets".into(), number: 7, title: "Fix it".into(), change }
    }

    fn run(result: Result<Outcome, String>) -> RunFinished {
        RunFinished { request: VerifyRequest { repo_id: "r".into(), pr_number: 7, check: "tests".into() }, result }
    }

    fn outcome(phase: Phase, code: i32, message: Option<&str>) -> Outcome {
        Outcome { phase, code, message: message.map(str::to_owned) }
    }

    #[test]
    fn a_hidden_popover_with_the_setting_on_gets_notified() {
        let state = ProductState::default();
        let found = notices(true, true, &[transition(Change::Failing)], &[], &state);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "#7 is failing");
        assert_eq!(found[0].body, "Fix it in acme/widgets");
    }

    #[test]
    fn a_visible_popover_or_the_setting_off_stays_quiet() {
        let state = ProductState::default();
        let all = [transition(Change::Ready)];
        let runs = [run(Ok(outcome(Phase::Signoff, 0, None)))];
        assert!(notices(true, false, &all, &runs, &state).is_empty(), "the popover shows it");
        assert!(notices(false, true, &all, &runs, &state).is_empty(), "switched off");
    }

    #[test]
    fn finished_runs_say_signed_off_or_failed_and_stopped_ones_say_nothing() {
        let state = ProductState::default();
        let runs = [
            run(Ok(outcome(Phase::Signoff, 0, None))),
            run(Ok(outcome(Phase::Run, 2, Some("tests exited with 2")))),
            run(Ok(outcome(Phase::Run, CANCELLED, Some("stopped before signing off")))),
            run(Err("map a local command to tests in Settings first".into())),
        ];
        let found = notices(true, true, &[], &runs, &state);
        let titles: Vec<&str> = found.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(titles, ["Signed off tests on #7", "tests failed on #7", "Could not run tests on #7"]);
        assert_eq!(found[1].body, "tests exited with 2");
    }
}
