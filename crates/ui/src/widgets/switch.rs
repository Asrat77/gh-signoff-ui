//! A compact on/off switch with a label, for settings inside a card (the
//! full-width [`ToggleRow`](super::ToggleRow) is for settings of their own).

use egui::{CornerRadius, CursorIcon, Rect, Response, Sense, Ui, Vec2, Widget};

use crate::theme::space;
use crate::{Type, palette};

/// A label and a pill that slides to the right when on. On is ember.
pub struct Switch<'a> {
    on: &'a mut bool,
    label: &'a str,
}

impl<'a> Switch<'a> {
    /// A switch that flips `on` when clicked.
    pub fn new(on: &'a mut bool, label: &'a str) -> Self {
        Self { on, label }
    }
}

impl Widget for Switch<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = palette(ui.ctx());
        let track = Vec2::new(44.0, 26.0);
        let galley = ui.painter().layout_no_wrap(self.label.to_owned(), Type::Row.font(), p.text);
        let size = Vec2::new(galley.size().x + space::M + track.x, track.y.max(galley.size().y));
        let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
        if response.clicked() {
            *self.on = !*self.on;
            response.mark_changed();
        }
        let on = *self.on;
        response
            .widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), on, self.label));
        let response = response.on_hover_cursor(CursorIcon::PointingHand);
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let enabled = ui.is_enabled();
            let text = if enabled { p.text } else { p.secondary };
            painter.galley(egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0), galley.clone(), text);
            let track_rect =
                Rect::from_min_size(egui::pos2(rect.right() - track.x, rect.center().y - track.y / 2.0), track);
            let fill = if on { p.accent } else { p.surface_active };
            let fill = if response.hovered() && enabled { crate::Palette::mix(fill, p.text, 0.08) } else { fill };
            painter.rect_filled(track_rect, CornerRadius::same(13), fill);
            let knob_x = if on { track_rect.right() - 13.0 } else { track_rect.left() + 13.0 };
            let knob = if on { p.on_accent } else { p.secondary };
            painter.circle_filled(egui::pos2(knob_x, track_rect.center().y), 9.0, knob);
        }
        response
    }
}
