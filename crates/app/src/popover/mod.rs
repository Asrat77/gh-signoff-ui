//! The glance popover: every tracked pull request at a glance, and each
//! one's required signoffs on its head commit.
//!
//! The popover is an ember card. Its band carries the eyes, which look
//! toward the trouble, and the sheet under it holds either the list or one
//! pull request's page. Everything shown comes from the latest state the
//! background refresh copied in, so drawing never waits on `gh`.

mod command_palette;
mod detail;
mod list;
mod parts;

use egui::{Frame, Ui};
use signoff_ui::palette;
use signoff_ui::theme::radius;

use crate::app::Signoff;
use crate::glance::Filter;

pub(crate) use command_palette::CommandPalette;

/// What the popover is showing.
#[derive(Debug, Default)]
pub(crate) struct Glance {
    /// The chip chosen above the list.
    pub(crate) filter: Filter,
    /// The page on the sheet.
    pub(crate) page: Page,
    /// The check picked on a pull request's page, to run.
    pub(crate) check: Option<String>,
    /// The command palette, while it is open.
    pub(crate) palette: Option<CommandPalette>,
}

/// The page the sheet shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Page {
    /// Every pull request.
    #[default]
    List,
    /// One pull request's signoffs.
    PullRequest {
        /// Its repository's id.
        repo_id: String,
        /// Its number.
        number: u64,
    },
}

/// Draws the popover into the window.
pub(crate) fn show(ui: &mut Ui, app: &mut Signoff) {
    let p = palette(ui.ctx());
    // A popover is a rounded card; an ordinary window keeps square corners.
    let corners = if app.is_popover() { radius::SHEET } else { 0 };
    Frame::new().fill(p.accent).corner_radius(corners).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        ui.spacing_mut().item_spacing.y = 0.0;
        match app.glance.page.clone() {
            Page::List => list::show(ui, app),
            Page::PullRequest { repo_id, number } => detail::show(ui, app, &repo_id, number),
        }
    });
    command_palette::show(ui, app);
}
