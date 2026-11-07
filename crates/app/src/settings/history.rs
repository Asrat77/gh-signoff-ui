//! History: what Signoff ran and wrote on this computer, beside the
//! statuses GitHub reported, newest first.

use egui::{RichText, Ui};
use jiff::Timestamp;
use signoff_core::{ActivityEvent, ActivityKind, CheckState, ProductState};
use signoff_ui::theme::space;
use signoff_ui::widgets::{Chip, LogView};
use signoff_ui::{Status, Type, icons, palette};

use super::parts;

/// Entries drawn; older ones stay in the file.
const SHOWN: usize = 100;

fn icon(kind: ActivityKind) -> &'static str {
    match kind {
        ActivityKind::Run => icons::TERMINAL,
        ActivityKind::Signoff => icons::CHECK,
        ActivityKind::Status => icons::COMMIT,
        ActivityKind::Rules => icons::BRANCH,
    }
}

fn status(state: CheckState) -> Status {
    match state {
        CheckState::Success => Status::Ready,
        CheckState::Pending => Status::Running,
        state if state.is_failed() => Status::Failing,
        _ => Status::Neutral,
    }
}

fn state_word(state: CheckState) -> &'static str {
    match state {
        CheckState::Success => "success",
        CheckState::Failure => "failure",
        CheckState::Error => "an error",
        CheckState::Pending => "pending",
        CheckState::Absent => "nothing",
    }
}

/// The first line of an entry.
fn title(event: &ActivityEvent) -> String {
    let what = match event.kind {
        ActivityKind::Run => format!("Ran {}", event.check),
        ActivityKind::Signoff => format!("Signed off {}", event.check),
        ActivityKind::Status => format!(
            "{} reported {}",
            if event.check.is_empty() { "GitHub" } else { &event.check },
            state_word(event.state)
        ),
        ActivityKind::Rules => "Changed branch rules".to_owned(),
    };
    if event.pr_number > 0 {
        format!("{what} · {} #{}", event.repo, event.pr_number)
    } else {
        format!("{what} · {}", event.repo)
    }
}

/// "3m ago" and the like.
fn ago(at: Timestamp, now: Timestamp) -> String {
    let seconds = now.duration_since(at).as_secs().max(0);
    match seconds {
        0..60 => "just now".to_owned(),
        60..3600 => format!("{}m ago", seconds / 60),
        3600..86_400 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

pub(super) fn show(ui: &mut Ui, state: &ProductState) {
    parts::heading(ui, "History", "Commands run and signoffs written from this computer, with what GitHub reported.");
    if state.history.is_empty() {
        parts::notice(ui, Status::Neutral, "Nothing yet. Runs, signoffs and rule changes show up here.");
        return;
    }
    let now = Timestamp::now();
    for event in state.history.iter().take(SHOWN) {
        parts::card(ui, |ui| {
            let p = palette(ui.ctx());
            let status = status(event.state);
            ui.horizontal_top(|ui| {
                ui.label(RichText::new(icon(event.kind)).font(signoff_ui::fonts::icon(16.0)).color(status.color(&p)));
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(title(event)).font(Type::Row.font()).color(p.text));
                        ui.label(RichText::new(ago(event.at, now)).font(Type::Caption.font()).color(p.secondary));
                    });
                    let mut detail = Vec::new();
                    if !event.actor.is_empty() {
                        detail.push(format!("by {}", event.actor));
                    }
                    if event.kind == ActivityKind::Run && event.duration_millis > 0 {
                        detail.push(format!(
                            "exit {} in {:.1}s",
                            event.exit_code,
                            event.duration_millis as f64 / 1000.0
                        ));
                    }
                    if !event.detail.is_empty() {
                        detail.push(event.detail.clone());
                    }
                    if !detail.is_empty() {
                        parts::caption(ui, &detail.join(" · "));
                    }
                    if !event.command.is_empty() {
                        ui.label(RichText::new(&event.command).font(Type::Mono.font()).color(p.secondary));
                    }
                    output(ui, event);
                });
            });
        });
    }
    if state.history.len() > SHOWN {
        ui.add_space(space::S);
        parts::caption(ui, &format!("Showing the latest {SHOWN}."));
    }
}

/// A "Show output" toggle, and the output under it, for an entry that
/// kept what it printed.
fn output(ui: &mut Ui, event: &ActivityEvent) {
    let lines = crate::runs::past_log(event);
    if lines.is_empty() {
        return;
    }
    let id = ui.id().with(("history-output", &event.id));
    let mut open = ui.data(|data| data.get_temp::<bool>(id)).unwrap_or(false);
    let (label, icon) = if open { ("Hide output", icons::REMOVE) } else { ("Show output", icons::TERMINAL) };
    if ui.add(Chip::new(label).icon(icon)).clicked() {
        open = !open;
        ui.data_mut(|data| data.insert_temp(id, open));
    }
    if open {
        let (result, status) = crate::runs::past_result(event);
        let title = format!("{} output", event.check);
        ui.add(LogView::new(&title, &lines).phase(&result, status).max_height(220.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::SignedDuration;

    #[test]
    fn times_read_as_ages() {
        let now = Timestamp::now();
        assert_eq!(ago(now, now), "just now");
        assert_eq!(ago(now - SignedDuration::from_mins(5), now), "5m ago");
        assert_eq!(ago(now - SignedDuration::from_hours(30), now), "1d ago");
    }

    #[test]
    fn titles_name_the_pull_request() {
        let event = ActivityEvent {
            kind: ActivityKind::Signoff,
            check: "tests".into(),
            repo: "o/r".into(),
            pr_number: 7,
            ..ActivityEvent::default()
        };
        assert_eq!(title(&event), "Signed off tests · o/r #7");
    }
}
