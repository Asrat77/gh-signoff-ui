//! Chips: filters at the top of the list and pills in the settings nav.

use egui::{CornerRadius, CursorIcon, Response, Sense, Ui, Vec2, Widget};

use super::paint::labeled;
use crate::theme::{radius, size, space};
use crate::{Palette, Type, palette};

/// A rounded pill that can be selected, with an optional icon and count.
pub struct Chip<'a> {
    label: &'a str,
    icon: Option<&'a str>,
    count: Option<usize>,
    selected: bool,
    width: Option<f32>,
}

impl<'a> Chip<'a> {
    /// A chip reading `label`.
    #[must_use]
    pub const fn new(label: &'a str) -> Self {
        Self { label, icon: None, count: None, selected: false, width: None }
    }

    /// An icon before the label, as in the settings nav.
    #[must_use]
    pub const fn icon(mut self, icon: &'a str) -> Self {
        self.icon = Some(icon);
        self
    }

    /// A count after the label.
    #[must_use]
    pub const fn count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    /// Whether it is the chosen one.
    #[must_use]
    pub const fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// A fixed width with the label on the left, for a nav column.
    #[must_use]
    pub const fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }
}

impl Widget for Chip<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = palette(ui.ctx());
        let font = Type::Button.font();
        let (ink, fill) = colors(&p, self.selected, self.width.is_some());
        let label = labeled(ui, self.icon, self.label, None, &font, ink);
        let count = self
            .count
            .map(|count| ui.painter().layout_no_wrap(count.to_string(), font.clone(), ink.gamma_multiply(0.6)));
        let content = label.size().x + count.as_ref().map_or(0.0, |c| c.size().x + 7.0);
        let height = if self.width.is_some() { size::CHIP + 6.0 } else { size::CHIP };
        let width = self.width.unwrap_or(content + 2.0 * (space::M + 2.0));
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
        let response = response.on_hover_cursor(CursorIcon::PointingHand);
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, ui.is_enabled(), self.selected, self.label)
        });
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let fill = if response.hovered() && !self.selected { p.surface_hover } else { fill };
            painter.rect_filled(rect, CornerRadius::same(radius::PILL), fill);
            let left =
                if self.width.is_some() { rect.left() + space::L - 2.0 } else { rect.center().x - content / 2.0 };
            let top = rect.center().y - label.size().y / 2.0;
            let label_width = label.size().x;
            painter.galley(egui::pos2(left, top), label, ink);
            if let Some(count) = count {
                painter.galley(egui::pos2(left + label_width + 7.0, top), count, ink);
            }
        }
        response
    }
}

fn colors(p: &Palette, selected: bool, nav: bool) -> (egui::Color32, egui::Color32) {
    match (selected, nav) {
        (true, _) => (p.on_primary, p.primary),
        (false, true) => (p.secondary, egui::Color32::TRANSPARENT),
        (false, false) => (p.text, p.surface),
    }
}
