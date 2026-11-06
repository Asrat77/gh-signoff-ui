//! A quiet line that says whether what is shown is current.

use egui::{Response, Sense, Ui, Widget, vec2};

use crate::{Status, Type, palette};

/// Dot size and the gap after it.
const DOT: f32 = 6.0;
const GAP: f32 = 6.0;

/// A small dot in a status colour and a few words, centred: "Live",
/// "Reading…", "Offline · retrying in 20s". The dot breathes while
/// [`LiveIndicator::busy`] is set.
pub struct LiveIndicator<'a> {
    status: Status,
    text: &'a str,
    busy: bool,
}

impl<'a> LiveIndicator<'a> {
    /// `text` after a dot in `status`'s colour.
    #[must_use]
    pub const fn new(status: Status, text: &'a str) -> Self {
        Self { status, text, busy: false }
    }

    /// Something is happening right now: the dot fades in and out.
    #[must_use]
    pub const fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }
}

impl Widget for LiveIndicator<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = palette(ui.ctx());
        let galley = ui.painter().layout_no_wrap(self.text.to_owned(), Type::Caption.font(), p.secondary);
        let height = galley.size().y.max(DOT);
        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
        let width = DOT + GAP + galley.size().x;
        let left = rect.center().x - width / 2.0;
        let mut color = self.status.color(&p);
        if self.busy {
            let t = ui.input(|i| i.time) as f32;
            color = color.gamma_multiply(0.55 + 0.45 * (t * 3.0).sin().abs());
            ui.ctx().request_repaint();
        }
        ui.painter().circle_filled(egui::pos2(left + DOT / 2.0, rect.center().y), DOT / 2.0, color);
        let text_at = egui::pos2(left + DOT + GAP, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(text_at, galley, p.secondary);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, self.text));
        response
    }
}
