//! Drawing helpers the widgets share.

use std::sync::Arc;

use egui::text::{LayoutJob, TextWrapping};
use egui::{Align2, Color32, CornerRadius, Galley, Painter, Pos2, Rect, Stroke, Ui, Vec2};

use crate::Type;
use crate::theme::{motion, radius};

/// `text` in the `step` font, cut with an ellipsis at `max_width`.
pub(crate) fn line(ui: &Ui, text: impl Into<String>, step: Type, color: Color32, max_width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.into(), step.font(), color);
    job.wrap = TextWrapping::truncate_at_width(max_width.max(0.0));
    ui.painter().layout_job(job)
}

/// `text` with an optional icon before and after it, each icon drawn in
/// the icon font and the text in `font`, on one line.
pub(crate) fn labeled(
    ui: &Ui,
    leading: Option<&str>,
    text: &str,
    trailing: Option<&str>,
    font: &egui::FontId,
    color: Color32,
) -> Arc<Galley> {
    let mut job = LayoutJob::default();
    let icon = crate::fonts::icon(font.size * 1.15);
    let format = |font: egui::FontId| egui::text::TextFormat {
        font_id: font,
        color,
        valign: egui::Align::Center,
        ..Default::default()
    };
    if let Some(leading) = leading {
        job.append(leading, 0.0, format(icon.clone()));
        job.append(text, font.size * 0.5, format(font.clone()));
    } else {
        job.append(text, 0.0, format(font.clone()));
    }
    if let Some(trailing) = trailing {
        job.append(trailing, font.size * 0.5, format(icon));
    }
    ui.painter().layout_job(job)
}

/// An icon centred on a rounded tile.
pub fn icon_tile(painter: &Painter, rect: Rect, icon: &str, fg: Color32, bg: Color32) {
    painter.rect_filled(rect, CornerRadius::same(radius::TILE), bg);
    painter.text(rect.center(), Align2::CENTER_CENTER, icon, crate::fonts::icon(rect.height() * 0.46), fg);
}

/// The running spinner: an arc that turns, or a still one when motion is
/// reduced.
pub fn spinner(ui: &Ui, center: Pos2, radius: f32, color: Color32) {
    let still = crate::theme::reduced_motion(ui.ctx());
    let time = if still { 0.0 } else { ui.input(|i| i.time) };
    let turn = (time / f64::from(motion::SPIN)).fract();
    #[allow(clippy::cast_possible_truncation)]
    let start = (turn as f32) * std::f32::consts::TAU;
    let points: Vec<Pos2> = (0..=24)
        .map(|i| {
            #[allow(clippy::cast_precision_loss)]
            let angle = start + (i as f32 / 24.0) * std::f32::consts::TAU * 0.72;
            center + radius * Vec2::angled(angle)
        })
        .collect();
    ui.painter().add(egui::Shape::line(points, Stroke::new(radius * 0.32, color)));
    if !still {
        ui.ctx().request_repaint();
    }
}
