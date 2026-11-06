//! A setting that is on or off. An "on" row is filled ember.

use egui::{Align2, CornerRadius, CursorIcon, Rect, Response, Sense, Ui, Vec2, Widget};

use super::paint::line;
use crate::theme::{radius, space};
use crate::{Palette, Type, icons, palette};

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
            let (fill, text, hint) = if on {
                (p.accent, p.on_accent, p.on_accent.gamma_multiply(0.85))
            } else {
                (p.surface, p.text, p.secondary)
            };
            let fill = if response.hovered() { Palette::mix(fill, text, 0.05) } else { fill };
            let painter = ui.painter();
            painter.rect_filled(rect, CornerRadius::same(radius::ROW), fill);
            let dot = 26.0;
            let left = rect.left() + space::L + 2.0;
            let width = rect.right() - space::L - dot - space::M - left;
            let label = line(ui, self.label, Type::Row, text, width);
            let detail = line(ui, self.hint, Type::Caption, hint, width);
            let block = label.size().y + 3.0 + detail.size().y;
            let top = rect.center().y - block / 2.0;
            let detail_top = top + label.size().y + 3.0;
            painter.galley(egui::pos2(left, top), label, text);
            painter.galley(egui::pos2(left, detail_top), detail, hint);
            let center = egui::pos2(rect.right() - space::L - dot / 2.0, rect.center().y);
            let knob = Rect::from_center_size(center, Vec2::splat(dot));
            if on {
                painter.circle_filled(knob.center(), dot / 2.0, p.on_accent);
                painter.text(knob.center(), Align2::CENTER_CENTER, icons::CHECK, crate::fonts::icon(14.0), p.accent);
            } else {
                painter.circle_filled(knob.center(), dot / 2.0, p.surface_active);
            }
        }
        response
    }
}
