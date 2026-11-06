//! A required signoff on a pull request's page.

use egui::{Align2, Color32, CornerRadius, Rect, Response, Sense, Ui, Vec2};

use super::paint::{icon_tile, line, spinner};
use crate::theme::{radius, size, space};
use crate::{Palette, Status, Type, icons, palette};

/// One required signoff: its name, the local command that proves it, and
/// where it stands. The check that needs you is filled ember.
pub struct CheckRow<'a> {
    name: &'a str,
    command: &'a str,
    category: &'a str,
    status: Status,
}

/// What happened to a [`CheckRow`] this frame.
pub struct CheckRowResponse {
    /// The whole row.
    pub response: Response,
}

impl<'a> CheckRow<'a> {
    /// A check called `name` that runs `command`.
    #[must_use]
    pub const fn new(name: &'a str, command: &'a str, status: Status) -> Self {
        Self { name, command, category: name, status }
    }

    /// The `gh signoff` category, which picks the icon. Defaults to the
    /// name.
    #[must_use]
    pub const fn category(mut self, category: &'a str) -> Self {
        self.category = category;
        self
    }

    /// Draws the row.
    pub fn show(self, ui: &mut Ui) -> CheckRowResponse {
        let p = palette(ui.ctx());
        let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), size::ROW), Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{}: {}", self.name, self.status.label()))
        });
        if ui.is_rect_visible(rect) {
            self.paint(ui, rect, &p, response.hovered());
        }
        CheckRowResponse { response }
    }

    fn paint(&self, ui: &Ui, rect: Rect, p: &Palette, hovered: bool) {
        let loud = matches!(self.status, Status::Attention | Status::Running);
        let (fill, text, secondary, tile) = if loud {
            (p.accent, p.on_accent, p.on_accent.gamma_multiply(0.82), Color32::from_black_alpha(50))
        } else {
            (p.surface, p.text, p.secondary, p.surface_active)
        };
        let fill = if hovered { Palette::mix(fill, text, 0.05) } else { fill };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(radius::ROW), fill);
        let tile_rect = Rect::from_center_size(
            egui::pos2(rect.left() + space::M + size::TILE / 2.0, rect.center().y),
            Vec2::splat(size::TILE),
        );
        icon_tile(painter, tile_rect, icons::for_category(self.category), text, tile);

        let dot = 24.0;
        let left = tile_rect.right() + space::M;
        let width = rect.right() - space::L - dot - space::M - left;
        let name = line(ui, self.name, Type::Row, text, width);
        let command = line(ui, self.command, Type::Mono, secondary, width);
        let block = name.size().y + 4.0 + command.size().y;
        let top = rect.center().y - block / 2.0;
        let command_top = top + name.size().y + 4.0;
        painter.galley(egui::pos2(left, top), name, text);
        painter.galley(egui::pos2(left, command_top), command, secondary);

        let center = egui::pos2(rect.right() - space::L - dot / 2.0, rect.center().y);
        let (dot_fill, dot_ink) = match self.status {
            Status::Ready => (p.success, Palette::ink_on(p.success)),
            Status::Failing => (p.danger, p.on_accent),
            Status::Attention | Status::Running => (p.on_accent, p.accent),
            Status::Neutral => (p.surface_active, p.secondary),
        };
        painter.circle_filled(center, dot / 2.0, dot_fill);
        if self.status == Status::Running {
            spinner(ui, center, 6.0, dot_ink);
        } else {
            painter.text(center, Align2::CENTER_CENTER, self.status.icon(), crate::fonts::icon(13.0), dot_ink);
        }
    }
}
