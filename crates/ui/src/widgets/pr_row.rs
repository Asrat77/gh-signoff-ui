//! A pull request in the popover's list.

use egui::{CornerRadius, CursorIcon, Rect, Response, Sense, Ui, Vec2, Widget};

use super::paint::{icon_tile, line};
use super::{Segment, Segments};
use crate::theme::{radius, size, space};
use crate::{Palette, Status, Type, palette};

/// One pull request: its state, title, where it lives, and how many of its
/// signoffs pass. Failing rows are filled with the danger colour.
pub struct PrRow<'a> {
    status: Status,
    title: &'a str,
    repo: &'a str,
    number: u64,
    branch: &'a str,
    checks: &'a [Segment],
}

impl<'a> PrRow<'a> {
    /// A row for pull request `number` in `repo`.
    #[must_use]
    pub const fn new(status: Status, title: &'a str, repo: &'a str, number: u64) -> Self {
        Self { status, title, repo, number, branch: "", checks: &[] }
    }

    /// The head branch.
    #[must_use]
    pub const fn branch(mut self, branch: &'a str) -> Self {
        self.branch = branch;
        self
    }

    /// Its required signoffs, in order.
    #[must_use]
    pub const fn checks(mut self, checks: &'a [Segment]) -> Self {
        self.checks = checks;
        self
    }
}

impl Widget for PrRow<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = palette(ui.ctx());
        let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), size::ROW), Sense::click());
        let response = response.on_hover_cursor(CursorIcon::PointingHand);
        let passed = self.checks.iter().filter(|segment| **segment == Segment::Done).count();
        let summary = format!("{passed}/{}", self.checks.len());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                true,
                format!("{}: {} #{}, {}, {summary} signoffs", self.status.label(), self.repo, self.number, self.title),
            )
        });
        if !ui.is_rect_visible(rect) {
            return response;
        }
        let colors = self.status.row_colors(&p);
        let fill = if response.hovered() { Palette::mix(colors.fill, colors.text, 0.05) } else { colors.fill };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(radius::ROW), fill);

        let tile = Rect::from_center_size(
            egui::pos2(rect.left() + space::M + size::TILE / 2.0, rect.center().y),
            Vec2::splat(size::TILE),
        );
        icon_tile(painter, tile, self.status.icon(), colors.glyph, colors.tile);

        let right_width = 14.0 * 3.0 + 6.0;
        let text_left = tile.right() + space::M;
        let text_width = rect.right() - space::L - right_width - space::M - text_left;
        let title = line(ui, self.title, Type::Row, colors.text, text_width);
        let mut meta = format!("{} #{}", self.repo, self.number);
        if !self.branch.is_empty() {
            meta.push_str("  ·  ");
            meta.push_str(self.branch);
        }
        let meta = line(ui, meta, Type::Caption, colors.secondary, text_width);
        let block = title.size().y + 4.0 + meta.size().y;
        let top = rect.center().y - block / 2.0;
        let meta_top = top + title.size().y + 4.0;
        painter.galley(egui::pos2(text_left, top), title, colors.text);
        painter.galley(egui::pos2(text_left, meta_top), meta, colors.secondary);

        let count = ui.painter().layout_no_wrap(summary, Type::Row.sized(12.0), colors.text);
        let right = rect.right() - space::L;
        painter.galley(egui::pos2(right - count.size().x, rect.center().y - count.size().y - 1.0), count, colors.text);
        let bar = Segments::small(self.checks).on_fill(self.status.is_loud());
        #[allow(clippy::cast_precision_loss)]
        let count = self.checks.len() as f32;
        let bar_width = count * 14.0 + (count - 1.0).max(0.0) * 3.0;
        let bar_rect =
            Rect::from_min_size(egui::pos2(right - bar_width, rect.center().y + 5.0), Vec2::new(bar_width, 5.0));
        bar.paint(ui, bar_rect, response.id.with("segments"));
        response
    }
}
