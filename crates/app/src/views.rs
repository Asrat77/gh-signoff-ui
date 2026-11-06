//! Placeholder views: enough to see the core working end to end.
//!
//! The glance popover and the settings window are designed and built in
//! their own steps; these stay deliberately plain until then.

use egui::{RichText, ViewportBuilder, ViewportId};
use signoff_core::{Environment, Health, ProductState, PullRequestState};

use crate::app::Signoff;
use signoff_ui::{Palette, Status};

/// The main window's contents.
pub(crate) fn popover(ui: &mut egui::Ui, app: &mut Signoff) {
    let palette = signoff_ui::palette(ui.ctx());
    egui::Frame::new().inner_margin(16.0).show(ui, |ui| {
        header(ui, app, &palette);
        ui.add_space(8.0);
        if app.demo {
            ui.label(RichText::new("Demo data: nothing is read from or written to GitHub.").color(palette.secondary));
            ui.add_space(8.0);
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| body(ui, &app.state, &palette));
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
            .color(status(summary.status).color(palette)),
        );
    }
}

fn body(ui: &mut egui::Ui, state: &ProductState, palette: &Palette) {
    if state.fetched_at.is_none() {
        ui.label(RichText::new("Loading…").color(palette.secondary));
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
        ui.colored_label(palette.danger, error);
    }
    for repo in state.repositories.iter().filter(|r| r.error.is_some()) {
        ui.colored_label(palette.danger, format!("{}: {}", repo.name_with_owner, repo.error.as_deref().unwrap_or("")));
    }
    if state.pull_requests.is_empty() {
        ui.label(RichText::new("No open pull requests.").color(palette.secondary));
    }
    for pr in &state.pull_requests {
        pull_request_row(ui, pr, palette);
        ui.add_space(6.0);
    }
}

/// The design system's state for a health.
pub(crate) fn status(health: Health) -> Status {
    match health {
        Health::Failing => Status::Failing,
        Health::Attention => Status::Attention,
        Health::Ready => Status::Ready,
        Health::Neutral => Status::Neutral,
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

fn pull_request_row(ui: &mut egui::Ui, pr: &PullRequestState, palette: &Palette) {
    egui::Frame::new().fill(palette.surface).corner_radius(8.0).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 4.5, status(pr.status).color(palette));
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
        ui.label(RichText::new(format!("{} · {progress}", pr.repo)).color(palette.secondary));
    });
}

/// The settings window, as its own native window.
pub(crate) fn settings_window(ctx: &egui::Context, app: &mut Signoff) {
    let id = ViewportId::from_hash_of("settings");
    let builder = ViewportBuilder::default().with_title("Signoff Settings").with_inner_size([520.0, 420.0]);
    ctx.show_viewport_immediate(id, builder, |ui, _class| {
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            app.settings_open = false;
        }
        let palette = signoff_ui::palette(ui.ctx());
        egui::Frame::new().inner_margin(16.0).show(ui, |ui| {
            ui.heading("Settings");
            ui.add_space(8.0);
            if app.demo {
                ui.label(RichText::new("Settings are read-only in demo mode.").color(palette.secondary));
            } else {
                ui.label(format!("Config file: {}", app.state.config_path.display()));
            }
            ui.add_space(8.0);
            ui.strong("Repositories");
            if app.state.repositories.is_empty() {
                ui.label(RichText::new("None yet.").color(palette.secondary));
            }
            for repo in &app.state.repositories {
                ui.label(format!("{}  ({})", repo.name_with_owner, repo.path.display()));
            }
            ui.add_space(8.0);
            ui.label(
                RichText::new("Adding repositories and mapping commands come in a later step.")
                    .color(palette.secondary),
            );
        });
    });
}
