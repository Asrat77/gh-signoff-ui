//! Short confirmations at the bottom of the window that fade away.

use egui::{Align2, Area, Context, CornerRadius, Frame, Id, Margin, Order};

use crate::theme::{self, radius};
use crate::{Status, Type, palette};

/// A message waiting to be shown.
#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    /// What it says.
    pub text: String,
    /// Its state, which picks the glyph.
    pub status: Status,
    /// When it appeared, in egui's input time.
    pub shown_at: f64,
}

const LIFETIME: f64 = 3.2;
const FADE: f64 = 0.25;

fn key() -> Id {
    Id::new("signoff-ui-toasts")
}

/// Queues `text` as a toast.
pub fn toast(ctx: &Context, text: impl Into<String>, status: Status) {
    let now = ctx.input(|input| input.time);
    ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<Vec<Toast>>(key()).push(Toast { text: text.into(), status, shown_at: now });
    });
    ctx.request_repaint();
}

/// Draws the toasts still showing. Call once per frame, after the panels.
pub fn show_toasts(ctx: &Context) {
    let now = ctx.input(|input| input.time);
    let mut toasts = ctx.data_mut(|data| data.get_temp::<Vec<Toast>>(key()).unwrap_or_default());
    toasts.retain(|toast| now - toast.shown_at < LIFETIME);
    ctx.data_mut(|data| data.insert_temp(key(), toasts.clone()));
    let Some(latest) = toasts.last() else {
        return;
    };
    let p = palette(ctx);
    let age = now - latest.shown_at;
    let fade = if theme::reduced_motion(ctx) { 1.0 } else { (age / FADE).min((LIFETIME - age) / FADE).clamp(0.0, 1.0) };
    #[allow(clippy::cast_possible_truncation)]
    let opacity = fade as f32;
    Area::new(key()).order(Order::Tooltip).anchor(Align2::CENTER_BOTTOM, [0.0, -20.0]).interactable(false).show(
        ctx,
        |ui| {
            ui.set_opacity(opacity);
            Frame::new()
                .fill(p.primary)
                .corner_radius(CornerRadius::same(radius::BUTTON))
                .inner_margin(Margin::symmetric(16, 12))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(latest.status.icon())
                                .font(crate::fonts::icon(15.0))
                                .color(p.on_primary),
                        );
                        ui.label(egui::RichText::new(&latest.text).font(Type::Button.font()).color(p.on_primary));
                    });
                });
        },
    );
    ctx.request_repaint();
}
