//! One pull request's page: its required signoffs and where each stands
//! on the exact head commit, and running one locally to sign it off.

use egui::Response;
use egui::{Align, Frame, Layout, Margin, RichText, ScrollArea, Ui};
use signoff_core::{ActivityKind, Check, CheckState, Ending, Outcome, Phase, PullRequestState, VerifyRequest};
use signoff_ui::theme::{radius, size, space};
use signoff_ui::widgets::{
    Button, ButtonKind, CheckRow, Chip, EmptyState, Headline, IconButton, IconButtonKind, LogLine, LogView, Mood,
    Segments, sheet, toast,
};
use signoff_ui::{Status, Type, icons, palette};

use super::Page;
use super::parts::{band, caption, height_above, notice, refresh_button};
use crate::app::Signoff;
use crate::checkouts::Checkout;
use crate::failure::{self, Failure};
use crate::glance;
use crate::runs::{self, RunView};

/// Room kept under the checks for the actions.
const FOOTER: f32 = 116.0;

/// More room for the actions when "Check out #N" sits under the main
/// button: a compact button and the gap above it.
const SECOND_BUTTON: f32 = size::CHIP + 6.0 + space::S;

/// Draws the page for the pull request `number` in `repo_id`, or sends
/// the popover back to the list once it is gone (merged or closed).
pub(super) fn show(ui: &mut Ui, app: &mut Signoff, repo_id: &str, number: u64) {
    let Some(pr) = app.state.pull_request(repo_id, number).cloned() else {
        app.glance.page = Page::List;
        return super::list::show(ui, app);
    };
    let target = target(app, &pr).cloned();
    let run = target.as_ref().and_then(|check| app.runs.view(&request(&pr, check)));
    let running = run.as_ref().is_some_and(RunView::running);
    let mood = if running { Mood::Busy } else { Mood::for_status(glance::status(pr.status)) };
    band(ui, mood, app.demo, |ui| {
        if ui.add(IconButton::new(icons::EXTERNAL, "Open on GitHub")).clicked() {
            crate::browser::open(ui.ctx(), &pr.pr.url);
        }
        super::parts::palette_button(ui, app);
        refresh_button(ui, app);
    });
    sheet(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.set_height(ui.available_height());
        Frame::new().inner_margin(Margin { left: 18, right: 18, top: 18, bottom: 12 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                let back =
                    IconButton::new(icons::BACK, "Back to pull requests").kind(IconButtonKind::OnSheet).size(36.0);
                if ui.add(back).clicked() || ui.input(|i| i.key_pressed(egui::Key::Backspace)) {
                    app.glance.page = Page::List;
                }
                ui.add_space(space::XS);
                let segments = glance::page_segments(&pr);
                if !segments.is_empty() {
                    ui.add(Segments::new(&segments));
                }
            });
            ui.add_space(space::L);
            ui.add(Headline::new(&pr.pr.title).detail(&glance::progress(&pr)).title_size().centered());
            ui.add_space(space::S);
            head(ui, &pr);
        });
        Frame::new().inner_margin(Margin::symmetric(14, 0)).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = space::S;
            let extra = if offers_checkout(app, &pr, target.as_ref(), run.as_ref()) { SECOND_BUTTON } else { 0.0 };
            let height = height_above(ui, FOOTER + extra);
            ScrollArea::vertical().max_height(height).auto_shrink([false, false]).show(ui, |ui| {
                problems(ui, &pr);
                let required: Vec<&Check> = glance::required(&pr).collect();
                if required.is_empty() {
                    let mut set_up = false;
                    ui.add(
                        EmptyState::new(
                            Mood::Relaxed,
                            "No signoffs required",
                            "This branch's protection asks for no signoff checks.",
                        )
                        .action("Set up signoff", &mut set_up),
                    );
                    if set_up {
                        let branch = if pr.rules.branch.is_empty() { "main" } else { &pr.rules.branch };
                        app.settings.set_up_signoff(&pr.repo_id, branch);
                        app.settings_open = true;
                    }
                }
                for check in required {
                    let row = check_row(ui, app, &pr, check, target.as_ref());
                    if check.configured {
                        let row = row.on_hover_cursor(egui::CursorIcon::PointingHand);
                        if row.clicked() {
                            app.glance.check = Some(check.name.clone());
                        }
                    }
                    if !check.configured && check.state != CheckState::Success {
                        let chip = Chip::new("Configure").icon(icons::TERMINAL);
                        if ui.add(chip).on_hover_text(format!("Map a local command to {}", check.name)).clicked() {
                            configure(app, &pr, &check.name);
                        }
                    }
                }
                if let (Some(check), Some(run)) = (&target, &run) {
                    ui.add_space(space::XS);
                    let failure = run.result.as_ref().and_then(|result| failure::describe(&check.name, result));
                    if let Some(failure) = &failure {
                        failure_card(ui, app, &pr, check, run, failure);
                    }
                    if !run.lines.is_empty() {
                        run_log(ui, check, run);
                    }
                    if failure.is_none()
                        && !run.running()
                        && ui.add(Chip::new("Clear output").icon(icons::REMOVE)).clicked()
                    {
                        app.runs.dismiss(&request(&pr, check));
                    }
                } else if let (Some(check), None) = (&target, &run)
                    && picked(app, check)
                {
                    last_run(ui, app, &pr, check);
                }
            });
            ui.add_space(space::M);
            actions(ui, app, &pr, target.as_ref(), run.as_ref());
        });
    });
}

/// Where the pull request lives and the commit its checks are read on.
fn head(ui: &mut Ui, pr: &PullRequestState) {
    let p = palette(ui.ctx());
    ui.with_layout(Layout::top_down(Align::Center), |ui| {
        ui.horizontal(|ui| {
            let repo = RichText::new(format!("{} #{}", pr.repo.rsplit('/').next().unwrap_or(&pr.repo), pr.pr.number))
                .font(Type::Caption.font());
            let commit =
                RichText::new(format!("{} {} @ {}", icons::COMMIT, pr.pr.head_ref, glance::short_sha(&pr.pr.head_sha)))
                    .font(Type::Mono.font());
            // Centre the line by measuring it first.
            let width = ui.fonts_mut(|fonts| {
                let a = fonts.layout_no_wrap(repo.text().to_owned(), Type::Caption.font(), p.secondary).size().x;
                let b = fonts.layout_no_wrap(commit.text().to_owned(), Type::Mono.font(), p.secondary).size().x;
                a + b.min(ui.available_width() - a - space::M).max(0.0) + space::M
            });
            ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
            ui.label(repo.color(p.secondary));
            // A long branch name shortens to fit rather than run off the edge.
            ui.add(egui::Label::new(commit.color(p.secondary)).truncate())
                .on_hover_text(format!("{} @ {}", pr.pr.head_ref, pr.pr.head_sha));
        });
    });
}

/// Whether `check` was picked by tapping its row, rather than being the
/// one the merge happens to wait on.
fn picked(app: &Signoff, check: &Check) -> bool {
    app.glance.check.as_deref() == Some(check.name.as_str())
}

/// What the last local run of `check` on this pull request did, from
/// history, for a check with no run in this session: its output when
/// history kept it, else how it ended.
fn last_run(ui: &mut Ui, app: &Signoff, pr: &PullRequestState, check: &Check) {
    let last = app.state.history.iter().find(|event| {
        event.kind == ActivityKind::Run
            && event.repo_id == pr.repo_id
            && event.pr_number == pr.pr.number
            && event.check == check.name
    });
    let Some(event) = last else {
        let text = format!("{} has not been run from here on this pull request yet.", check.name);
        return notice(ui, Status::Neutral, &text);
    };
    let when = event.at.strftime("%Y-%m-%d %H:%M").to_string();
    let lines = runs::past_log(event);
    if !lines.is_empty() {
        let (result, status) = runs::past_result(event);
        let title = format!("{} · last run {when}", check.name);
        ui.add(LogView::new(&title, &lines).phase(&result, status).max_height(150.0));
        return;
    }
    // Runs from before history kept output.
    let outcome = if event.exit_code == 0 { "passed" } else { "failed" };
    let secs = event.duration_millis as f64 / 1000.0;
    let text =
        format!("Last run {when}: {outcome} (exit {}) in {secs:.1}s. Run it again to see its output.", event.exit_code);
    notice(ui, if event.exit_code == 0 { Status::Ready } else { Status::Failing }, &text);
}

/// The check the page's button acts on: the one running, else the one
/// picked, else the one the merge waits on, if it has a local command.
fn target<'a>(app: &Signoff, pr: &'a PullRequestState) -> Option<&'a Check> {
    let mut required = glance::required(pr);
    let running = |check: &&Check| app.runs.view(&request(pr, check)).is_some_and(|run| run.running());
    if let Some(check) = glance::required(pr).find(running) {
        return Some(check);
    }
    if let Some(name) = &app.glance.check
        && let Some(check) = glance::required(pr).find(|check| &check.name == name)
    {
        return Some(check);
    }
    required.find(|check| pr.blocking.as_deref() == Some(&check.name) && check.configured)
}

/// Whether the main button runs `check`: it has a local command and is not
/// signed off yet, or it is signed off and was picked to run once more.
fn runnable(app: &Signoff, check: &Check) -> bool {
    check.configured && (check.state != CheckState::Success || picked(app, check))
}

/// The check the merge waits on when it has no local command and no other
/// check is picked: the page's button then offers to set one up.
fn to_configure<'a>(pr: &'a PullRequestState, target: Option<&Check>) -> Option<&'a Check> {
    if target.is_some() {
        return None;
    }
    glance::required(pr).find(|check| pr.blocking.as_deref() == Some(&check.name) && !check.configured)
}

/// Opens Settings → Commands on `check` of this pull request's repository.
fn configure(app: &mut Signoff, pr: &PullRequestState, check: &str) {
    app.settings.configure_check(&pr.repo_id, check);
    app.settings_open = true;
}

/// The run request for `check` on `pr`.
fn request(pr: &PullRequestState, check: &Check) -> VerifyRequest {
    VerifyRequest { repo_id: pr.repo_id.clone(), pr_number: pr.pr.number, check: check.name.clone() }
}

/// One required signoff on the head commit; a running one spins.
fn check_row(ui: &mut Ui, app: &Signoff, pr: &PullRequestState, check: &Check, target: Option<&Check>) -> Response {
    let command = if check.configured { check.local_command.as_str() } else { check.command.as_str() };
    let running = app.runs.view(&request(pr, check)).is_some_and(|run| run.running());
    let status = if running { Status::Running } else { glance::check_status(pr, check) };
    // The picked check is the loud one, so the button below reads right.
    let status = match (status, target) {
        (Status::Neutral, Some(target)) if target.name == check.name => Status::Attention,
        (Status::Attention, Some(target)) if target.name != check.name => Status::Neutral,
        (status, _) => status,
    };
    let row = CheckRow::new(&check.name, command, status).category(&check.category);
    let response = row.show(ui).response;
    if check.actor.is_empty() {
        response.on_hover_text(format!("{} has no signoff on this commit yet", check.context))
    } else {
        let by = match check.at {
            Some(at) => format!("{} by {} at {}", check.context, check.actor, at.strftime("%Y-%m-%d %H:%M")),
            None => format!("{} by {}", check.context, check.actor),
        };
        response.on_hover_text(by)
    }
}

/// A run's output under the checks, newest at the bottom.
fn run_log(ui: &mut Ui, check: &Check, run: &RunView) {
    let mut lines = Vec::with_capacity(run.lines.len() + 3);
    if run.dropped > 0 {
        lines.push(LogLine::Step(format!("… {} earlier lines", run.dropped)));
    }
    lines.extend(run.lines.iter().cloned().map(LogLine::Output));
    let (phase, status) = match &run.result {
        None => (
            match run.phase {
                Phase::Prepare => "preparing worktree",
                Phase::Checkout => "checking commit",
                Phase::Run => "streaming",
                Phase::Signoff => "signing off",
            }
            .to_owned(),
            Status::Running,
        ),
        Some(Ok(outcome)) if outcome.signed_off() => {
            lines.push(LogLine::Success(format!("{} signed off {}", icons::CHECK, check.name)));
            ("signed off".to_owned(), Status::Ready)
        }
        // The card above says what happened; the log ends the way a
        // terminal would.
        Some(Ok(outcome)) => match outcome.ending {
            Ending::CheckFailed => {
                lines.push(LogLine::Failure(format!("exit {}", outcome.code)));
                (format!("exit {}", outcome.code), Status::Failing)
            }
            Ending::TimedOut => {
                lines.push(LogLine::Failure("timed out".to_owned()));
                ("timed out".to_owned(), Status::Failing)
            }
            Ending::Stopped => {
                lines.push(LogLine::Step("stopped".to_owned()));
                ("stopped".to_owned(), Status::Neutral)
            }
            Ending::Interrupted | Ending::SignedOff => ("not signed off".to_owned(), Status::Attention),
        },
        Some(Err(_)) => ("didn't start".to_owned(), Status::Attention),
    };
    let title = format!("{} output", check.name);
    ui.add(LogView::new(&title, &lines).phase(&phase, status).max_height(150.0));
}

/// What a run that did not sign off means, and what to do next: for a
/// failed check, hand it to a coding agent or copy its output; open it in
/// History or dismiss it from the corner. The main button below runs it
/// again. The card scrolls into view once, when the run ends.
fn failure_card(
    ui: &mut Ui,
    app: &mut Signoff,
    pr: &PullRequestState,
    check: &Check,
    run: &RunView,
    failure: &Failure,
) {
    let p = palette(ui.ctx());
    let accent = failure.status.color(&p);
    let outcome = run.result.as_ref().and_then(|result| result.as_ref().ok());
    let entry = outcome.and_then(|outcome| outcome.entry.clone());
    let lines: Vec<String> = run.lines.iter().cloned().collect();
    // A tint of the status colour, so the chips' own fill reads as pills.
    let card = Frame::new()
        .fill(accent.gamma_multiply(0.08))
        .stroke(egui::Stroke::new(1.0, accent.gamma_multiply(0.55)))
        .corner_radius(radius::ROW)
        .inner_margin(Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = space::S;
                ui.label(RichText::new(failure.status.icon()).font(signoff_ui::fonts::icon(17.0)).color(accent));
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.x = space::XS;
                    let dismiss = IconButton::new(icons::REMOVE, "Dismiss").kind(IconButtonKind::OnSheet).size(28.0);
                    if ui.add(dismiss).clicked() {
                        app.runs.dismiss(&request(pr, check));
                    }
                    if let Some(entry) = &entry {
                        let history =
                            IconButton::new(icons::HISTORY, "Open in History").kind(IconButtonKind::OnSheet).size(28.0);
                        if ui.add(history).clicked() {
                            app.settings.open_history(Some(entry.clone()));
                            app.settings_open = true;
                        }
                    }
                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        ui.label(RichText::new(&failure.title).font(Type::Row.font()).color(p.text).strong());
                        ui.add_space(2.0);
                        let detail = RichText::new(&failure.detail).font(Type::Caption.font()).color(p.secondary);
                        ui.add(egui::Label::new(detail).wrap());
                    });
                });
            });
            let copy_agent = failure.check_failed && outcome.is_some();
            if !copy_agent && lines.is_empty() && !failure.timed_out {
                return;
            }
            ui.add_space(space::S);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(space::XS, space::XS);
                if let Some(outcome) = outcome.filter(|_| copy_agent) {
                    let chip = Chip::new("Copy for agent").icon(icons::AGENT);
                    let hover =
                        "Copy a prompt with the command, the commit and the output, to paste into a coding agent";
                    if ui.add(chip).on_hover_text(hover).clicked() {
                        ui.ctx().copy_text(failure::agent_prompt(pr, &check.name, outcome, &lines));
                        toast(ui.ctx(), "Copied a prompt for your coding agent", Status::Ready);
                    }
                }
                if !lines.is_empty() && ui.add(Chip::new("Copy output").icon(icons::COPY)).clicked() {
                    ui.ctx().copy_text(failure::plain_output(outcome, &lines));
                    toast(ui.ctx(), "Copied the output", Status::Ready);
                }
                if failure.timed_out && ui.add(Chip::new("Change timeout").icon(icons::TERMINAL)).clicked() {
                    configure(app, pr, &check.name);
                }
            });
        });
    // Once per ending: a new run that fails again scrolls again.
    let seen = egui::Id::new(("failure-seen", &pr.id, &check.name, run.lines.len(), entry));
    if !ui.data(|data| data.get_temp::<bool>(seen).unwrap_or(false)) {
        ui.data_mut(|data| data.insert_temp(seen, true));
        card.response.scroll_to_me(Some(Align::TOP));
    }
}

/// Whether the latest run of `check` here failed the check itself.
fn check_failed(run: Option<&RunView>) -> bool {
    run.and_then(|run| run.result.as_ref()).is_some_and(|result| result.as_ref().is_ok_and(Outcome::check_failed))
}

/// What stops this pull request's checks from being trusted as shown.
fn problems(ui: &mut Ui, pr: &PullRequestState) {
    if let Some(error) = &pr.error {
        notice(ui, Status::Failing, error);
    }
    if let Some(warning) = &pr.warning {
        notice(ui, Status::Attention, warning);
    }
    if !pr.rules.readable {
        notice(ui, Status::Attention, "Branch protection could not be read; required checks may be missing.");
    } else if !pr.rules.enforced && pr.total > 0 {
        notice(ui, Status::Attention, "Branch protection lists these checks but does not enforce them.");
    }
}

/// Whether the page is not on the pull request's head, so a run happens in
/// Signoff's own worktree, and the checkout can be switched there instead.
fn isolated(app: &Signoff, pr: &PullRequestState) -> bool {
    let at_head = pr.checked_out || app.checkouts.simulated(&pr.repo_id, pr.pr.number);
    // With the setting on, a checkout elsewhere is no reason to stop: the
    // run happens in Signoff's own worktree at the head.
    !at_head && !app.demo && app.settings.isolated_worktrees_on()
}

/// Whether "Check out #N" is offered under "Run & sign off": a run would
/// use Signoff's worktree, and none is going.
fn offers_checkout(app: &Signoff, pr: &PullRequestState, target: Option<&Check>, run: Option<&RunView>) -> bool {
    isolated(app, pr)
        && target.is_some_and(|check| check.configured && check.state != CheckState::Success)
        && !run.is_some_and(RunView::running)
        && !pr.repo_path.as_os_str().is_empty()
}

/// The page's main button: run and sign off the target check, stop it
/// while it runs, or open a ready pull request to merge.
fn actions(ui: &mut Ui, app: &mut Signoff, pr: &PullRequestState, target: Option<&Check>, run: Option<&RunView>) {
    let number = pr.pr.number;
    let checkout = app.checkouts.view(&pr.repo_id, number);
    let at_head = pr.checked_out || app.checkouts.simulated(&pr.repo_id, number);
    let isolated = isolated(app, pr);
    let unmapped = to_configure(pr, target);
    let confirming = app.glance.confirm_checkout.as_ref().is_some_and(|(id, n)| *id == pr.repo_id && *n == number);
    match (target, run) {
        (Some(check), Some(run)) if run.running() => {
            let label = format!("Stop {}", check.name);
            if ui.add(Button::new(&label).kind(ButtonKind::Secondary).icon(icons::STOP)).clicked() {
                app.runs.stop(&request(pr, check));
            }
        }
        (Some(check), _) if runnable(app, check) && !at_head && !isolated => {
            if check_out_button(ui, app, pr, checkout.as_ref(), ButtonKind::Primary) {
                app.checkouts.start(&pr.repo_id, number, pr.repo_path.clone());
            }
        }
        (Some(check), _) if runnable(app, check) => {
            let again = check.state == CheckState::Success || check_failed(run);
            let label =
                if again { format!("Run {} again", check.name) } else { format!("Run & sign off {}", check.name) };
            let hover = if isolated {
                format!(
                    "Run `{}` in Signoff's worktree at {}, then sign off {} on this commit; your checkout stays as it is",
                    check.local_command,
                    glance::short_sha(&pr.pr.head_sha),
                    check.name
                )
            } else {
                format!("Run `{}`, then sign off {} on this commit", check.local_command, check.name)
            };
            let button = ui.add(Button::new(&label).icon(icons::RUN)).on_hover_text(hover);
            if button.clicked() {
                app.glance.confirm_checkout = None;
                app.runs.start(request(pr, check));
            }
        }
        _ if unmapped.is_some() => {
            let name = unmapped.map(|check| check.name.as_str()).unwrap_or_default();
            if ui.add(Button::new(&format!("Configure {name}")).icon(icons::TERMINAL)).clicked() {
                configure(app, pr, name);
            }
        }
        _ => {
            let (label, kind) = if pr.ready {
                ("Open pull request to merge", ButtonKind::Accent)
            } else {
                ("Open on GitHub", ButtonKind::Secondary)
            };
            if ui.add(Button::new(label).kind(kind).trailing_icon(icons::FORWARD)).clicked() {
                crate::browser::open(ui.ctx(), &pr.pr.url);
            }
        }
    }
    ui.add_space(space::S);
    let offered = offers_checkout(app, pr, target, run);
    let hint = match &checkout {
        Some(Checkout::Failed(message)) => message.clone(),
        Some(Checkout::Running) => format!("Running gh pr checkout {number}…"),
        _ if run.is_some_and(|run| run.running() && run.phase == Phase::Prepare) => "Preparing worktree…".to_owned(),
        _ if offered && confirming => format!("{}?", switch(app, pr)),
        _ if check_failed(run) => "Fix what it found, then run it again to sign off.".to_owned(),
        _ if isolated && unmapped.is_none() => format!(
            "Runs in a separate worktree at {}; your checkout is not touched.",
            glance::short_sha(&pr.pr.head_sha)
        ),
        _ => hint(app, pr, target, unmapped),
    };
    caption(ui, &hint);
    if offered {
        ui.add_space(space::S);
        check_out_instead(ui, app, pr, checkout.as_ref(), confirming);
    }
}

/// "Check out #N" under "Run & sign off". Checking out switches the
/// person's own checkout, so a click first asks to confirm it.
fn check_out_instead(
    ui: &mut Ui,
    app: &mut Signoff,
    pr: &PullRequestState,
    checkout: Option<&Checkout>,
    confirm: bool,
) {
    let number = pr.pr.number;
    let busy = matches!(checkout, Some(Checkout::Running | Checkout::Done));
    if !confirm || busy {
        if check_out_button(ui, app, pr, checkout, ButtonKind::Secondary) {
            app.glance.confirm_checkout = Some((pr.repo_id.clone(), number));
        }
        return;
    }
    let half = ((ui.available_width() - space::S) / 2.0).max(0.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::S;
        let cancel = Button::new("Cancel").kind(ButtonKind::Secondary).compact().width(half);
        if ui.add(cancel).clicked() {
            app.glance.confirm_checkout = None;
        }
        let switch_branch =
            Button::new("Switch branch").kind(ButtonKind::Secondary).icon(icons::BRANCH).compact().width(half);
        if ui.add(switch_branch).on_hover_text(checkout_hover(app, pr)).clicked() {
            app.glance.confirm_checkout = None;
            app.checkouts.start(&pr.repo_id, number, pr.repo_path.clone());
        }
    });
}

/// The button that checks out the pull request, disabled while that runs:
/// the main button (`kind` primary), or a compact one under it. Returns
/// whether it was clicked.
fn check_out_button(
    ui: &mut Ui,
    app: &Signoff,
    pr: &PullRequestState,
    checkout: Option<&Checkout>,
    kind: ButtonKind,
) -> bool {
    let number = pr.pr.number;
    let (label, busy) = match checkout {
        Some(Checkout::Running) => (format!("Checking out #{number}…"), true),
        Some(Checkout::Done) => (format!("Reading #{number}…"), true),
        _ => (format!("Check out #{number}"), false),
    };
    let mut button =
        Button::new(&label).kind(kind).icon(if busy { icons::RUNNING } else { icons::BRANCH }).enabled(!busy);
    if kind != ButtonKind::Primary {
        button = button.compact();
    }
    ui.add(button).on_hover_text(checkout_hover(app, pr)).clicked()
}

/// What checking out does to the person's checkout, such as "This
/// switches gh-signoff from main to fix-flaky-tests".
fn switch(app: &Signoff, pr: &PullRequestState) -> String {
    let dir = pr.repo_path.file_name().map_or_else(|| pr.repo.clone(), |name| name.to_string_lossy().into_owned());
    let current = app
        .state
        .repositories
        .iter()
        .find(|repo| repo.id == pr.repo_id)
        .map(|repo| repo.current_branch.as_str())
        .filter(|branch| !branch.is_empty() && *branch != pr.pr.head_ref);
    match current {
        Some(current) => format!("This switches {dir} from {current} to {}", pr.pr.head_ref),
        None => format!("This switches {dir} to {}", pr.pr.head_ref),
    }
}

/// The hover text of the buttons that check the pull request out.
fn checkout_hover(app: &Signoff, pr: &PullRequestState) -> String {
    format!(
        "{} with `gh pr checkout {}` in {}; refused if tracked files have uncommitted changes",
        switch(app, pr),
        pr.pr.number,
        pr.repo_path.display()
    )
}

/// The line under the main button when nothing is running or failed.
fn hint(app: &Signoff, pr: &PullRequestState, target: Option<&Check>, unmapped: Option<&Check>) -> String {
    match target {
        _ if app.demo => "Demo: runs are simulated and nothing is signed off on GitHub.".to_owned(),
        _ if unmapped.is_some() => {
            format!(
                "Map a local command to {} to run it from here.",
                unmapped.map(|c| c.name.as_str()).unwrap_or_default()
            )
        }
        _ if pr.checked_out => "Your checkout is on this commit.".to_owned(),
        _ => format!("Check out {} at {} to verify it locally.", pr.pr.head_ref, glance::short_sha(&pr.pr.head_sha)),
    }
}
