//! Placeholder views: enough to see the core working end to end.
//!
//! The glance popover and the settings window are designed and built in
//! their own steps; these stay deliberately plain until then.

use egui::{RichText, ViewportBuilder, ViewportId};
use signoff_core::{Check, CheckState, Environment, Health, Phase, ProductState, PullRequestState, VerifyRequest};

use crate::app::Signoff;
use crate::runs::{RunView, Runs};
use crate::theme::Palette;

/// The main window's contents.
pub(crate) fn popover(ui: &mut egui::Ui, app: &mut Signoff) {
    let palette = Palette::current(ui.ctx());
    egui::Frame::new().inner_margin(16.0).show(ui, |ui| {
        header(ui, app, &palette);
        ui.add_space(8.0);
        if app.demo {
            ui.label(RichText::new("Demo data: nothing is read from or written to GitHub.").color(palette.muted));
            ui.add_space(8.0);
        }
        let demo = app.demo;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| body(ui, &app.state, &app.runs, demo, &palette));
    });
}

fn header(ui: &mut egui::Ui, app: &mut Signoff, palette: &Palette) {
    ui.horizontal(|ui| {
        ui.heading("Signoff");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Settings").clicked() {
                app.settings_open = true;
            }
            let refreshing = app.refreshing();
            if ui.add_enabled(!refreshing, egui::Button::new("Refresh")).clicked() {
                app.refresh();
            }
            if refreshing {
                ui.spinner();
            }
        });
    });
    let summary = &app.state.summary;
    if summary.total_pull_requests > 0 {
        ui.label(
            RichText::new(format!(
                "{} ready · {} need attention · {} open",
                summary.ready, summary.needs_attention, summary.total_pull_requests
            ))
            .color(palette.health(summary.status)),
        );
    }
}

fn body(ui: &mut egui::Ui, state: &ProductState, runs: &Runs, demo: bool, palette: &Palette) {
    if state.fetched_at.is_none() {
        ui.label(RichText::new("Loading…").color(palette.muted));
        return;
    }
    if let Some(problem) = setup_problem(&state.env) {
        ui.label(problem);
        return;
    }
    if !state.onboarded {
        ui.label("No repositories tracked yet. Open Settings to add a local checkout.");
        return;
    }
    if let Some(error) = &state.error {
        ui.colored_label(palette.failing, error);
    }
    for repo in state.repositories.iter().filter(|r| r.error.is_some()) {
        ui.colored_label(palette.failing, format!("{}: {}", repo.name_with_owner, repo.error.as_deref().unwrap_or("")));
    }
    if state.pull_requests.is_empty() {
        ui.label(RichText::new("No open pull requests.").color(palette.muted));
    }
    for pr in &state.pull_requests {
        pull_request_row(ui, pr, runs, demo, palette);
        ui.add_space(6.0);
    }
}

/// What must be fixed before there is anything to show, if anything.
fn setup_problem(env: &Environment) -> Option<&'static str> {
    if !env.gh_installed {
        Some("Install the GitHub CLI (gh) to use Signoff: https://cli.github.com")
    } else if !env.authenticated {
        Some("Sign in to GitHub: run `gh auth login`, then refresh.")
    } else {
        None
    }
}

fn pull_request_row(ui: &mut egui::Ui, pr: &PullRequestState, runs: &Runs, demo: bool, palette: &Palette) {
    egui::Frame::new().fill(palette.surface).corner_radius(8.0).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 4.5, palette.health(pr.status));
            ui.hyperlink_to(RichText::new(format!("#{} {}", pr.pr.number, pr.pr.title)).strong(), &pr.pr.url);
        });
        let progress = match (pr.status, pr.total) {
            (_, 0) => "No signoff required".to_owned(),
            (Health::Ready, _) => "Ready to merge".to_owned(),
            (_, total) => match &pr.blocking {
                Some(blocking) => format!("{}/{total} signed off · waiting on {blocking}", pr.passed),
                None => format!("{}/{total} signed off", pr.passed),
            },
        };
        ui.label(RichText::new(format!("{} · {progress}", pr.repo)).color(palette.muted));
        if !pr.checks.is_empty() {
            ui.add_space(4.0);
        }
        for check in &pr.checks {
            check_row(ui, pr, check, runs, demo, palette);
        }
    });
}

fn check_state_label(state: CheckState) -> &'static str {
    match state {
        CheckState::Success => "signed off",
        CheckState::Failure => "failed",
        CheckState::Error => "error",
        CheckState::Pending => "pending",
        CheckState::Absent => "not signed off",
    }
}

fn check_colour(state: CheckState, palette: &Palette) -> egui::Color32 {
    match state {
        CheckState::Success => palette.ready,
        CheckState::Failure | CheckState::Error => palette.failing,
        CheckState::Pending => palette.attention,
        CheckState::Absent => palette.muted,
    }
}

/// One check, with its verify button and, once run, its output.
fn check_row(ui: &mut egui::Ui, pr: &PullRequestState, check: &Check, runs: &Runs, demo: bool, palette: &Palette) {
    let request = VerifyRequest { repo_id: pr.repo_id.clone(), pr_number: pr.pr.number, check: check.name.clone() };
    let run = runs.view(&request);
    let running = run.as_ref().is_some_and(RunView::running);

    ui.horizontal(|ui| {
        let name = if check.required { RichText::new(&check.name).strong() } else { RichText::new(&check.name) };
        ui.label(name);
        ui.label(RichText::new(check_state_label(check.state)).color(check_colour(check.state, palette)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if running {
                if ui.small_button("Stop").clicked() {
                    runs.stop(&request);
                }
                ui.spinner();
            } else if !check.configured {
                ui.label(RichText::new("no local command").color(palette.muted).small());
            } else {
                let can_run = demo || pr.checked_out;
                let button = ui
                    .add_enabled(can_run, egui::Button::new("Verify").small())
                    .on_hover_text(format!("Run `{}`, then sign off {}", check.local_command, check.name))
                    .on_disabled_hover_text("Check out this pull request's head commit to verify it locally");
                if button.clicked() {
                    runs.start(request.clone());
                }
            }
        });
    });
    if let Some(run) = run {
        run_log(ui, &run, runs, palette);
    }
}

/// A run's output, newest at the bottom.
fn run_log(ui: &mut egui::Ui, run: &RunView, runs: &Runs, palette: &Palette) {
    egui::Frame::new().fill(palette.window).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let (text, colour) = match &run.result {
                None => (
                    match run.phase {
                        Phase::Run => "Running…",
                        Phase::Checkout => "Confirming the commit…",
                        Phase::Signoff => "Signing off…",
                    }
                    .to_owned(),
                    palette.attention,
                ),
                Some(Ok(outcome)) if outcome.signed_off() => ("Signed off".to_owned(), palette.ready),
                Some(Ok(outcome)) => {
                    (outcome.message.clone().unwrap_or_else(|| format!("exit {}", outcome.code)), palette.failing)
                }
                Some(Err(refused)) => (refused.clone(), palette.failing),
            };
            ui.label(RichText::new(text).color(colour).strong());
            if run.result.is_some() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Dismiss").clicked() {
                        runs.dismiss(&run.request);
                    }
                });
            }
        });
        if run.lines.is_empty() {
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt(("run-log", &run.request.repo_id, run.request.pr_number, &run.request.check))
            .max_height(180.0)
            .stick_to_bottom(true)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                if run.dropped > 0 {
                    ui.label(RichText::new(format!("… {} earlier lines", run.dropped)).color(palette.muted).small());
                }
                for line in &run.lines {
                    ui.label(RichText::new(strip_ansi(line)).monospace().small());
                }
            });
    });
}

/// Drops ANSI escape sequences (colours, cursor moves) that build tools
/// print; the design system's log viewer will render colours instead.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.next_if_eq(&'[').is_some() {
            // CSI: parameters and intermediates, then one final byte.
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        } else {
            chars.next();
        }
    }
    out
}

/// The settings window, as its own native window.
pub(crate) fn settings_window(ctx: &egui::Context, app: &mut Signoff) {
    let id = ViewportId::from_hash_of("settings");
    let builder = ViewportBuilder::default().with_title("Signoff Settings").with_inner_size([520.0, 420.0]);
    ctx.show_viewport_immediate(id, builder, |ui, _class| {
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            app.settings_open = false;
        }
        let palette = Palette::current(ui.ctx());
        egui::Frame::new().inner_margin(16.0).show(ui, |ui| {
            ui.heading("Settings");
            ui.add_space(8.0);
            if app.demo {
                ui.label(RichText::new("Settings are read-only in demo mode.").color(palette.muted));
            } else {
                ui.label(format!("Config file: {}", app.state.config_path.display()));
            }
            ui.add_space(8.0);
            ui.strong("Repositories");
            if app.state.repositories.is_empty() {
                ui.label(RichText::new("None yet.").color(palette.muted));
            }
            for repo in &app.state.repositories {
                ui.label(format!("{}  ({})", repo.name_with_owner, repo.path.display()));
            }
            ui.add_space(8.0);
            ui.label(
                RichText::new("Adding repositories and mapping commands come in a later step.").color(palette.muted),
            );
        });
    });
}

#[cfg(test)]
mod tests {
    use super::strip_ansi;

    #[test]
    fn strips_colour_codes() {
        assert_eq!(strip_ansi("\u{1b}[1;32mok\u{1b}[0m 12 tests"), "ok 12 tests");
        assert_eq!(strip_ansi("plain"), "plain");
    }
}
