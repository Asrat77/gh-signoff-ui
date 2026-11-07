//! A setting that is on or off. The row stays neutral; only its switch turns
//! ember when on, because a filled ember row reads as "failing, act here".

use egui::{CornerRadius, CursorIcon, Rect, Response, Sense, Ui, Vec2, Widget};

use super::paint::line;
use crate::theme::{radius, space};
use crate::{Palette, Type, palette};

/// A labelled on/off setting with a hint under it.
pub struct ToggleRow<'a> {
    on: &'a mut bool,
    label: &'a str,
    hint: &'a str,
}

impl<'a> ToggleRow<'a> {
    /// A row that flips `on` when clicked.
    pub fn new(on: &'a mut bool, label: &'a str, hint: &'a str) -> Self {
        Self { on, label, hint }
    }
}

impl Widget for ToggleRow<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = palette(ui.ctx());
        let (rect, mut response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 66.0), Sense::click());
        if response.clicked() {
            *self.on = !*self.on;
            response.mark_changed();
        }
        let on = *self.on;
        response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, self.label));
        let response = response.on_hover_cursor(CursorIcon::PointingHand);
        if ui.is_rect_visible(rect) {
            let enabled = ui.is_enabled();
            let text = if enabled { p.text } else { p.secondary };
            let fill = if response.hovered() && enabled { Palette::mix(p.surface, text, 0.05) } else { p.surface };
            let painter = ui.painter();
            painter.rect_filled(rect, CornerRadius::same(radius::ROW), fill);
            let track = Vec2::new(44.0, 26.0);
            let left = rect.left() + space::L + 2.0;
            let width = rect.right() - space::L - track.x - space::M - left;
            let label = line(ui, self.label, Type::Row, text, width);
            let detail = line(ui, self.hint, Type::Caption, p.secondary, width);
            let block = label.size().y + 3.0 + detail.size().y;
            let top = rect.center().y - block / 2.0;
            let detail_top = top + label.size().y + 3.0;
            painter.galley(egui::pos2(left, top), label, text);
            painter.galley(egui::pos2(left, detail_top), detail, p.secondary);
            let track_rect =
                Rect::from_center_size(egui::pos2(rect.right() - space::L - track.x / 2.0, rect.center().y), track);
            let track_fill = if on { p.accent } else { p.surface_active };
            painter.rect_filled(track_rect, CornerRadius::same(13), track_fill);
            let knob_x = if on { track_rect.right() - 13.0 } else { track_rect.left() + 13.0 };
            let knob = if on { p.on_accent } else { p.secondary };
            painter.circle_filled(egui::pos2(knob_x, track_rect.center().y), 9.0, knob);
        }
        response
    }
}
