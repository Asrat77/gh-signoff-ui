//! Segmented progress: one thick pill per required signoff.

use egui::{Color32, CornerRadius, Id, Rect, Response, Sense, Ui, Vec2, Widget};

use crate::theme::{self, motion, radius, size};
use crate::{Palette, Status, palette};

/// One segment of a [`Segments`] bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Segment {
    /// Signed off.
    Done,
    /// The step in progress or the one that needs you, in its state.
    Current(Status),
    /// Not done yet.
    Empty,
}

/// A row of pill segments, such as "2 of 3 signoffs".
pub struct Segments<'a> {
    segments: &'a [Segment],
    thickness: f32,
    width: Option<f32>,
    on_fill: bool,
}

impl<'a> Segments<'a> {
    /// A full-width bar of `segments`.
    #[must_use]
    pub const fn new(segments: &'a [Segment]) -> Self {
        Self { segments, thickness: size::SEGMENT, width: None, on_fill: false }
    }

    /// A small bar for the end of a row: 14 points per segment, 5 thick.
    #[must_use]
    pub fn small(segments: &'a [Segment]) -> Self {
        #[allow(clippy::cast_precision_loss)]
        let count = segments.len() as f32;
        Self { segments, thickness: 5.0, width: Some(count * 14.0 + (count - 1.0).max(0.0) * 3.0), on_fill: false }
    }

    /// Drawn on a filled (failing) row: white and translucent white.
    #[must_use]
    pub const fn on_fill(mut self, on_fill: bool) -> Self {
        self.on_fill = on_fill;
        self
    }

    /// Paints the bar into `rect`; used by rows that lay themselves out.
    pub fn paint(&self, ui: &Ui, rect: Rect, id: Id) {
        let p = palette(ui.ctx());
        let gap = if self.thickness < size::SEGMENT { 3.0 } else { 5.0 };
        #[allow(clippy::cast_precision_loss)]
        let count = self.segments.len().max(1) as f32;
        let each = (rect.width() - gap * (count - 1.0)) / count;
        let time = theme::duration(ui.ctx(), motion::GLIDE);
        for (i, segment) in self.segments.iter().enumerate() {
            let target = match segment {
                Segment::Done => {
                    if self.on_fill {
                        p.on_accent
                    } else {
                        p.text
                    }
                }
                Segment::Current(status) => {
                    if self.on_fill {
                        p.on_accent.gamma_multiply(0.6)
                    } else if matches!(status, Status::Attention | Status::Failing) {
                        p.accent
                    } else {
                        status.color(&p)
                    }
                }
                Segment::Empty => {
                    if self.on_fill {
                        Color32::from_white_alpha(80)
                    } else {
                        p.outline
                    }
                }
            };
            let done = ui.ctx().animate_bool_with_time(id.with(i), matches!(segment, Segment::Done), time);
            let color = if matches!(segment, Segment::Done) && !self.on_fill {
                Palette::mix(p.outline, target, done)
            } else {
                target
            };
            #[allow(clippy::cast_precision_loss)]
            let left = rect.left() + (each + gap) * i as f32;
            let segment_rect = Rect::from_min_size(
                egui::pos2(left, rect.center().y - self.thickness / 2.0),
                Vec2::new(each, self.thickness),
            );
            ui.painter().rect_filled(segment_rect, CornerRadius::same(radius::PILL), color);
        }
    }
}

impl Widget for Segments<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let width = self.width.unwrap_or_else(|| ui.available_width());
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, self.thickness), Sense::hover());
        if ui.is_rect_visible(rect) {
            self.paint(ui, rect, response.id);
        }
        response
    }
}
