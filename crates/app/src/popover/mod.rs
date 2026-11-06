//! The glance popover: every tracked pull request at a glance, and each
//! one's required signoffs on its head commit.
//!
//! The popover is an ember card. Its band carries the eyes, which look
//! toward the trouble, and the sheet under it holds either the list or one
//! pull request's page. Everything shown comes from the latest state the
//! background refresh copied in, so drawing never waits on `gh`.

mod list;
mod parts;

use egui::{Frame, Ui};
use signoff_ui::palette;
use signoff_ui::theme::radius;

use crate::app::Signoff;
use crate::glance::Filter;

/// What the popover is showing.
#[derive(Debug, Default)]
pub(crate) struct Glance {
    /// The chip chosen above the list.
    pub(crate) filter: Filter,
}

/// Draws the popover into the window.
pub(crate) fn show(ui: &mut Ui, app: &mut Signoff) {
    let p = palette(ui.ctx());
    Frame::new().fill(p.accent).corner_radius(radius::SHEET).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        ui.spacing_mut().item_spacing.y = 0.0;
        list::show(ui, app);
    });
}
