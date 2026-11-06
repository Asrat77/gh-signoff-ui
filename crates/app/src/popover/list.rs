//! The list: a headline, the filter chips, and every pull request sorted
//! failing first, then waiting on you, then ready.

use egui::{Frame, Margin, ScrollArea, Ui};
use jiff::Timestamp;
use signoff_core::{Health, ProductState, PullRequestState};
use signoff_ui::theme::space;
use signoff_ui::widgets::{Button, ButtonKind, Chip, EmptyState, Headline, IconButton, Mood, PrRow, sheet};
use signoff_ui::{Status, icons};

use super::Page;
use super::parts::{self, band, caption, notice, refresh_button};
use crate::app::Signoff;
use crate::glance::{self, Filter};

/// Room kept under the list for the main button and the "updated" line.
const FOOTER: f32 = 112.0;

/// What the list asks for this frame.
enum Action {
    /// Open this pull request's page.
    Page(Page),
}

/// Draws the list page.
pub(super) fn show(ui: &mut Ui, app: &mut Signoff) {
    let mood = Mood::for_status(glance::status(app.state.summary.status));
    band(ui, mood, app.demo, |ui| {
        if ui.add(IconButton::new(icons::SETTINGS, "Settings")).clicked() {
            app.settings_open = true;
        }
        refresh_button(ui, app);
    });
    let mut action = None;
    sheet(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.set_height(ui.available_height());
        if !blocked(ui, app) {
            action = body(ui, &app.state, &mut app.glance.filter);
        }
    });
    match action {
        Some(Action::Page(page)) => {
            app.glance.page = page;
            app.glance.check = None;
        }
        None => {}
    }
}

/// Shows why there is no list yet, if there is none; true when it did.
fn blocked(ui: &mut Ui, app: &mut Signoff) -> bool {
    let state = &app.state;
    let mut clicked = false;
    let mut open_settings = false;
    let shown = if state.fetched_at.is_none() {
        ui.add(EmptyState::new(Mood::Busy, "Looking around", "Reading your pull requests through gh…"));
        true
    } else if let Some(problem) = parts::setup_problem(&state.env) {
        ui.add(EmptyState::new(Mood::AtYou, problem.title, problem.detail).action(problem.action, &mut clicked));
        if clicked {
            match problem.url {
                Some(url) => ui.ctx().open_url(egui::OpenUrl::new_tab(url)),
                None => app.refresh(),
            }
        }
        true
    } else if !state.onboarded {
        if let Some(path) = app.settings.adding() {
            let detail = format!("Asking gh about {}…", path.display());
            ui.add(EmptyState::new(Mood::Busy, "Checking the folder", &detail));
        } else {
            ui.add(
                EmptyState::new(
                    Mood::AtYou,
                    "Track a repository",
                    "Choose the folder of a GitHub checkout to see its pull requests here.",
                )
                .action("Choose a folder…", &mut clicked),
            );
            ui.add_space(space::S);
            if ui.add(Button::new("Open settings").kind(ButtonKind::Secondary)).clicked() {
                open_settings = true;
            }
        }
        true
    } else {
        false
    };
    if clicked && !app.state.onboarded {
        let ctx = ui.ctx().clone();
        app.settings.choose_and_add(&ctx);
    }
    if open_settings {
        app.settings_open = true;
    }
    shown
}

fn body(ui: &mut Ui, state: &ProductState, filter: &mut Filter) -> Option<Action> {
    let mut action = None;
    Frame::new().inner_margin(Margin { left: 22, right: 22, top: 22, bottom: 14 }).show(ui, |ui| {
        let (title, detail) = glance::headline(state);
        ui.add(Headline::new(&title).detail(&detail));
    });
    Frame::new().inner_margin(Margin::symmetric(18, 0)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S - 2.0;
            for chip in Filter::ALL {
                let count = glance::count(state, chip);
                if ui.add(Chip::new(chip.label()).count(count).selected(*filter == chip)).clicked() {
                    *filter = chip;
                }
            }
        });
    });
    ui.add_space(space::M);
    Frame::new().inner_margin(Margin::symmetric(14, 0)).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = space::S;
        let height = parts::height_above(ui, FOOTER);
        ScrollArea::vertical().max_height(height).auto_shrink([false, false]).show(ui, |ui| {
            problems(ui, state);
            let prs = glance::visible(state, *filter);
            if prs.is_empty() {
                empty(ui, state, *filter);
            }
            for pr in prs {
                if row(ui, pr).clicked() {
                    action = Some(Action::Page(page_of(pr)));
                }
            }
        });
        ui.add_space(space::M);
        if let Some(pr) = glance::next_up(state, *filter) {
            let label = match pr.status {
                Health::Failing => format!("Fix #{} first", pr.pr.number),
                _ => format!("Sign off #{} next", pr.pr.number),
            };
            if ui.add(Button::new(&label).trailing_icon(icons::FORWARD)).clicked() {
                action = Some(Action::Page(page_of(pr)));
            }
        } else {
            ui.add(Button::new("Nothing waiting on you").kind(ButtonKind::Secondary).enabled(false));
        }
        ui.add_space(space::S);
        caption(ui, &glance::updated(state.fetched_at, Timestamp::now()));
    });
    // Keep "Updated 12s ago" honest while the window is open.
    ui.ctx().request_repaint_after_secs(5.0);
    action
}

/// The page for `pr`.
fn page_of(pr: &PullRequestState) -> Page {
    Page::PullRequest { repo_id: pr.repo_id.clone(), number: pr.pr.number }
}

/// One pull request's row.
fn row(ui: &mut Ui, pr: &PullRequestState) -> egui::Response {
    let segments = glance::row_segments(pr);
    let repo = pr.repo.rsplit('/').next().unwrap_or(&pr.repo);
    ui.add(
        PrRow::new(glance::status(pr.status), &pr.pr.title, repo, pr.pr.number)
            .branch(&pr.pr.head_ref)
            .checks(&segments),
    )
}

/// Errors that are not one pull request's: the refresh failing, or a
/// repository that could not be read.
fn problems(ui: &mut Ui, state: &ProductState) {
    if let Some(error) = &state.error {
        notice(ui, Status::Failing, error);
    }
    if let Some(warning) = &state.warning {
        notice(ui, Status::Attention, warning);
    }
    for repo in &state.repositories {
        if let Some(error) = &repo.error {
            notice(ui, Status::Failing, &format!("{}: {error}", repo.name_with_owner));
        } else if repo.pull_requests_truncated {
            let text =
                format!("{}: showing the first {} open pull requests.", repo.name_with_owner, repo.pull_request_count);
            notice(ui, Status::Attention, &text);
        }
    }
}

/// What an empty filter says.
fn empty(ui: &mut Ui, state: &ProductState, filter: Filter) {
    let (mood, title, detail) = match filter {
        _ if state.pull_requests.is_empty() => {
            (Mood::Relaxed, "Everything is clear", "No open pull requests in your tracked repositories.")
        }
        Filter::Yours => (Mood::Relaxed, "Nothing of yours", "None of the open pull requests are yours."),
        Filter::Failing => (Mood::Relaxed, "Nothing failing", "No required signoff has failed."),
        Filter::Ready => (Mood::AtYou, "Nothing ready yet", "No pull request has every signoff it needs."),
        Filter::All => (Mood::Relaxed, "Everything is clear", "No open pull requests."),
    };
    ui.add(EmptyState::new(mood, title, detail));
}
