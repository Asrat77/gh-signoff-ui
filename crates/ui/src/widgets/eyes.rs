//! The eyes: Signoff's mark, drawn in the band, on empty states and as
//! the tray icon. The pupils look toward the trouble.

use egui::{Color32, Id, Pos2, Response, Sense, Stroke, Ui, Vec2, Widget};

use crate::Status;
use crate::theme::{self, motion};

/// Where the eyes are looking, and why.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Mood {
    /// All clear: looking up, relaxed.
    Relaxed,
    /// Something needs you: looking straight at you.
    #[default]
    AtYou,
    /// Something failed: looking down at the list.
    Worried,
    /// A command is running: glancing sideways.
    Busy,
}

impl Mood {
    /// The mood for an overall health.
    #[must_use]
    pub const fn for_status(status: Status) -> Self {
        match status {
            Status::Failing => Self::Worried,
            Status::Attention | Status::Neutral => Self::AtYou,
            Status::Running => Self::Busy,
            Status::Ready => Self::Relaxed,
        }
    }

    /// Where the pupils sit, from -1 to 1 on each axis (y grows down).
    #[must_use]
    pub const fn gaze(self) -> Vec2 {
        match self {
            Self::Relaxed => Vec2::new(0.0, -0.75),
            Self::AtYou => Vec2::new(0.25, 0.1),
            Self::Worried => Vec2::new(0.35, 0.8),
            Self::Busy => Vec2::new(-0.85, 0.05),
        }
    }
}

/// A pair of eyes. `size` is the height of one eye.
#[derive(Clone, Copy, Debug)]
pub struct Eyes {
    mood: Mood,
    size: f32,
    id: Option<Id>,
}

impl Eyes {
    /// Eyes in `mood`, 24 points tall.
    #[must_use]
    pub const fn new(mood: Mood) -> Self {
        Self { mood, size: 24.0, id: None }
    }

    /// The height of one eye.
    #[must_use]
    pub const fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    /// A stable id, so the pupils glide from their last spot when the mood
    /// changes. Without one the id comes from the position in the layout.
    #[must_use]
    pub const fn id(mut self, id: Id) -> Self {
        self.id = Some(id);
        self
    }
}

impl Widget for Eyes {
    fn ui(self, ui: &mut Ui) -> Response {
        let eye = Vec2::new(self.size * 0.82, self.size);
        let gap = self.size * 0.12;
        let (rect, response) = ui.allocate_exact_size(Vec2::new(eye.x * 2.0 + gap, eye.y), Sense::hover());
        let id = self.id.unwrap_or(response.id);
        let ctx = ui.ctx();
        let time = theme::duration(ctx, motion::GLIDE);
        let target = self.mood.gaze();
        let gaze = Vec2::new(
            ctx.animate_value_with_time(id.with("x"), target.x, time),
            ctx.animate_value_with_time(id.with("y"), target.y, time),
        );
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let ink = Color32::from_rgb(0x11, 0x11, 0x11);
            for i in 0..2 {
                #[allow(clippy::cast_precision_loss)]
                let left = rect.left() + (eye.x + gap) * i as f32;
                let center = Pos2::new(left + eye.x / 2.0, rect.center().y);
                let radius = eye / 2.0;
                painter.add(egui::Shape::ellipse_filled(center, radius, Color32::WHITE));
                painter.add(egui::Shape::ellipse_stroke(
                    center,
                    radius - Vec2::splat(self.size * 0.04),
                    Stroke::new(self.size * 0.08, ink),
                ));
                let pupil = Vec2::new(eye.x * 0.24, eye.y * 0.24);
                let travel = radius - pupil - Vec2::splat(self.size * 0.1);
                painter.add(egui::Shape::ellipse_filled(center + gaze * travel, pupil, ink));
            }
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worried_eyes_look_down_and_relaxed_eyes_look_up() {
        assert!(Mood::Worried.gaze().y > 0.0);
        assert!(Mood::Relaxed.gaze().y < 0.0);
        assert_eq!(Mood::for_status(Status::Failing), Mood::Worried);
    }

    #[test]
    fn the_gaze_stays_inside_the_eye() {
        for mood in [Mood::Relaxed, Mood::AtYou, Mood::Worried, Mood::Busy] {
            let gaze = mood.gaze();
            assert!(gaze.x.abs() <= 1.0 && gaze.y.abs() <= 1.0, "{mood:?}");
        }
    }
}
