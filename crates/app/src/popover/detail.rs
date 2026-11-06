//! One pull request's page: its required signoffs and where each stands
//! on the exact head commit.

use egui::{Align, Frame, Layout, Margin, RichText, ScrollArea, Ui};
use signoff_core::{Check, PullRequestState};
use signoff_ui::theme::space;
use signoff_ui::widgets::{
    Button, ButtonKind, CheckRow, EmptyState, Headline, IconButton, IconButtonKind, Mood, Segments, sheet,
};
use signoff_ui::{Status, Type, icons, palette};

use super::Page;
use super::parts::{band, caption, height_above, notice, refresh_button};
use crate::app::Signoff;
use crate::glance;

/// Room kept under the checks for the actions.
const FOOTER: f32 = 92.0;

/// Draws the page for the pull request `number` in `repo_id`, or sends
/// the popover back to the list once it is gone (merged or closed).
pub(super) fn show(ui: &mut Ui, app: &mut Signoff, repo_id: &str, number: u64) {
    let Some(pr) = app.state.pull_request(repo_id, number).cloned() else {
        app.glance.page = Page::List;
        return super::list::show(ui, app);
    };
    let status = glance::status(pr.status);
    band(ui, Mood::for_status(status), app.demo, |ui| {
        if ui.add(IconButton::new(icons::EXTERNAL, "Open on GitHub")).clicked() {
            ui.ctx().open_url(egui::OpenUrl::new_tab(&pr.pr.url));
        }
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
                    ui.add(EmptyState::new(
                        Mood::Relaxed,
                        "No signoffs required",
                        "This branch's protection asks for no signoff checks.",
                    ));
                }
                for check in required {
                    check_row(ui, &pr, check);
                }
            });
            ui.add_space(space::M);
            actions(ui, &pr);
        });
    });
}

/// Where the pull request lives and the commit its checks are read on.
fn head(ui: &mut Ui, pr: &PullRequestState) {
    let p = palette(ui.ctx());
    ui.with_layout(Layout::top_down(Align::Center), |ui| {
        ui.horizontal(|ui| {
            let repo = RichText::new(format!("{} #{}", pr.repo, pr.pr.number)).font(Type::Caption.font());
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

/// One required signoff on the head commit.
fn check_row(ui: &mut Ui, pr: &PullRequestState, check: &Check) {
    let command = if check.configured { check.local_command.as_str() } else { check.command.as_str() };
    let row = CheckRow::new(&check.name, command, glance::check_status(pr, check)).category(&check.category);
    let response = row.show(ui).response;
    if check.actor.is_empty() {
        response.on_hover_text(format!("{} has no signoff on this commit yet", check.context));
    } else {
        let by = match check.at {
            Some(at) => format!("{} by {} at {}", check.context, check.actor, at.strftime("%Y-%m-%d %H:%M")),
            None => format!("{} by {}", check.context, check.actor),
        };
        response.on_hover_text(by);
    }
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

/// The page's main button. Verify and sign off adds its run controls here.
fn actions(ui: &mut Ui, pr: &PullRequestState) {
    let (label, kind) = if pr.ready {
        ("Open pull request to merge", ButtonKind::Accent)
    } else {
        ("Open on GitHub", ButtonKind::Primary)
    };
    if ui.add(Button::new(label).kind(kind).trailing_icon(icons::FORWARD)).clicked() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(&pr.pr.url));
    }
    ui.add_space(space::S);
    if pr.checked_out {
        caption(ui, "Your checkout is on this commit.");
    } else {
        caption(ui, &format!("Check out {} to verify it locally.", pr.pr.head_ref));
    }
}
