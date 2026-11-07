//! One pull request's page: its required signoffs and where each stands
//! on the exact head commit, and running one locally to sign it off.

use egui::Response;
use egui::{Align, Frame, Layout, Margin, RichText, ScrollArea, Ui};
use signoff_core::{Check, CheckState, Phase, PullRequestState, VerifyRequest};
use signoff_ui::theme::space;
use signoff_ui::widgets::{
    Button, ButtonKind, CheckRow, Chip, EmptyState, Headline, IconButton, IconButtonKind, LogLine, LogView, Mood,
    Segments, sheet,
};
use signoff_ui::{Status, Type, icons, palette};

use super::Page;
use super::parts::{band, caption, height_above, notice, refresh_button};
use crate::app::Signoff;
use crate::checkouts::Checkout;
use crate::glance;
use crate::runs::RunView;

/// Room kept under the checks for the actions.
const FOOTER: f32 = 92.0;

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
            ui.ctx().open_url(egui::OpenUrl::new_tab(&pr.pr.url));
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
            let height = height_above(ui, FOOTER);
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
                    if check_row(ui, app, &pr, check, target.as_ref()).clicked() && check.configured {
                        app.glance.check = Some(check.name.clone());
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
                    run_log(ui, check, run);
                    if !run.running() && ui.add(Chip::new("Clear output").icon(icons::REMOVE)).clicked() {
                        app.runs.dismiss(&request(&pr, check));
                    }
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
                a + b + space::M
            });
            ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
            ui.label(repo.color(p.secondary));
            ui.label(commit.color(p.secondary)).on_hover_text(&pr.pr.head_sha);
        });
    });
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
        Some(Ok(outcome)) => {
            let message = outcome.message.clone().unwrap_or_else(|| format!("exit {}", outcome.code));
            lines.push(LogLine::Failure(message));
            (format!("exit {}", outcome.code), Status::Failing)
        }
        Some(Err(refused)) => {
            lines.push(LogLine::Failure(refused.clone()));
            ("refused".to_owned(), Status::Failing)
        }
    };
    let title = format!("{} output", check.name);
    ui.add(LogView::new(&title, &lines).phase(&phase, status).max_height(150.0));
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

/// The page's main button: run and sign off the target check, stop it
/// while it runs, or open a ready pull request to merge.
fn actions(ui: &mut Ui, app: &mut Signoff, pr: &PullRequestState, target: Option<&Check>, run: Option<&RunView>) {
    let number = pr.pr.number;
    let checkout = app.checkouts.view(&pr.repo_id, number);
    let at_head = pr.checked_out || app.checkouts.simulated(&pr.repo_id, number);
    // With the setting on, a checkout elsewhere is no reason to stop: the
    // run happens in Signoff's own worktree at the head.
    let isolated = !at_head && !app.demo && app.settings.isolated_worktrees_on();
    let unmapped = to_configure(pr, target);
    match (target, run) {
        (Some(check), Some(run)) if run.running() => {
            let label = format!("Stop {}", check.name);
            if ui.add(Button::new(&label).kind(ButtonKind::Secondary).icon(icons::STOP)).clicked() {
                app.runs.stop(&request(pr, check));
            }
        }
        (Some(check), _) if check.configured && check.state != CheckState::Success && !at_head && !isolated => {
            check_out_button(ui, app, pr, checkout.as_ref());
        }
        (Some(check), _) if check.configured && check.state != CheckState::Success => {
            let label = format!("Run & sign off {}", check.name);
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
                ui.ctx().open_url(egui::OpenUrl::new_tab(&pr.pr.url));
            }
        }
    }
    ui.add_space(space::S);
    let hint = match checkout {
        Some(Checkout::Failed(message)) => message,
        Some(Checkout::Running) => format!("Running gh pr checkout {number}…"),
        _ if run.is_some_and(|run| run.running() && run.phase == Phase::Prepare) => "Preparing worktree…".to_owned(),
        _ if isolated && unmapped.is_none() => format!(
            "Runs in a separate worktree at {}; your checkout is not touched.",
            glance::short_sha(&pr.pr.head_sha)
        ),
        _ => hint(app, pr, target, unmapped),
    };
    caption(ui, &hint);
}

/// The button that checks out the pull request, disabled while that runs.
fn check_out_button(ui: &mut Ui, app: &Signoff, pr: &PullRequestState, checkout: Option<&Checkout>) {
    let number = pr.pr.number;
    let (label, busy) = match checkout {
        Some(Checkout::Running) => (format!("Checking out #{number}…"), true),
        Some(Checkout::Done) => (format!("Reading #{number}…"), true),
        _ => (format!("Check out #{number}"), false),
    };
    let button = Button::new(&label).icon(if busy { icons::RUNNING } else { icons::BRANCH }).enabled(!busy);
    let button = ui.add(button).on_hover_text(format!(
        "Run `gh pr checkout {number}` in {}; refused if tracked files have uncommitted changes",
        pr.repo_path.display()
    ));
    if button.clicked() {
        app.checkouts.start(&pr.repo_id, number, pr.repo_path.clone());
    }
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
