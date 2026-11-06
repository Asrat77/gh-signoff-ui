//! Buttons: the big main button, round icon buttons, and the dashed "add"
//! button.

use egui::{Align2, Color32, CornerRadius, CursorIcon, Response, Sense, Shape, Stroke, Ui, Vec2, Widget};

use super::paint::labeled;
use crate::theme::{radius, size};
use crate::{Palette, Type, palette};

/// Which colours a [`Button`] uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonKind {
    /// White on dark, black on light: the one thing to do next.
    #[default]
    Primary,
    /// Ember: a commitment, such as opening the PR to merge.
    Accent,
    /// A raised row colour: anything else.
    Secondary,
}

/// A big rounded button, full width unless given a width.
pub struct Button<'a> {
    text: &'a str,
    icon: Option<&'a str>,
    trailing_icon: Option<&'a str>,
    kind: ButtonKind,
    width: Option<f32>,
    height: f32,
    enabled: bool,
}

impl<'a> Button<'a> {
    /// A primary button.
    #[must_use]
    pub const fn new(text: &'a str) -> Self {
        Self {
            text,
            icon: None,
            trailing_icon: None,
            kind: ButtonKind::Primary,
            width: None,
            height: size::BUTTON,
            enabled: true,
        }
    }

    /// Its colours.
    #[must_use]
    pub const fn kind(mut self, kind: ButtonKind) -> Self {
        self.kind = kind;
        self
    }

    /// An icon before the text.
    #[must_use]
    pub const fn icon(mut self, icon: &'a str) -> Self {
        self.icon = Some(icon);
        self
    }

    /// An icon after the text, such as an arrow.
    #[must_use]
    pub const fn trailing_icon(mut self, icon: &'a str) -> Self {
        self.trailing_icon = Some(icon);
        self
    }

    /// A fixed width instead of the full width available.
    #[must_use]
    pub const fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// A smaller button, for inside a row or next to an input.
    #[must_use]
    pub const fn compact(mut self) -> Self {
        self.height = size::CHIP + 6.0;
        self
    }

    /// Greys it out and ignores clicks.
    #[must_use]
    pub const fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    fn colors(&self, p: &Palette, hovered: bool) -> (Color32, Color32) {
        let (fill, text) = match self.kind {
            ButtonKind::Primary => (p.primary, p.on_primary),
            ButtonKind::Accent => (p.accent, p.on_accent),
            ButtonKind::Secondary => (p.surface, p.text),
        };
        let fill = if !hovered {
            fill
        } else if self.kind == ButtonKind::Accent {
            p.accent_hover
        } else {
            Palette::mix(fill, text, 0.08)
        };
        (fill, text)
    }
}

impl Widget for Button<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = palette(ui.ctx());
        let width = self.width.unwrap_or_else(|| ui.available_width());
        let sense = if self.enabled { Sense::click() } else { Sense::hover() };
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, self.height), sense);
        let response = if self.enabled { response.on_hover_cursor(CursorIcon::PointingHand) } else { response };
        if ui.is_rect_visible(rect) {
            let (mut fill, mut ink) = self.colors(&p, response.hovered());
            if !self.enabled {
                fill = fill.gamma_multiply(0.45);
                ink = ink.gamma_multiply(0.6);
            }
            let painter = ui.painter();
            let corner = if self.height < size::BUTTON { radius::TILE } else { radius::BUTTON };
            painter.rect_filled(rect, CornerRadius::same(corner), fill);
            let label = labeled(ui, self.icon, self.text, self.trailing_icon, &Type::Button.sized(14.0), ink);
            let pos = rect.center() - label.size() / 2.0;
            ui.painter().galley(pos, label, ink);
        }
        response
    }
}

/// Where an [`IconButton`] sits, which decides its colours.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconButtonKind {
    /// On the ember band: a dark wash with a white icon.
    #[default]
    OnBand,
    /// On the sheet: a raised circle.
    OnSheet,
}

/// A round button with one icon. Give it an accessible label.
pub struct IconButton<'a> {
    icon: &'a str,
    label: &'a str,
    kind: IconButtonKind,
    size: f32,
}

impl<'a> IconButton<'a> {
    /// `icon`, announced to screen readers as `label`.
    #[must_use]
    pub const fn new(icon: &'a str, label: &'a str) -> Self {
        Self { icon, label, kind: IconButtonKind::OnBand, size: size::ICON_BUTTON }
    }

    /// Where it sits.
    #[must_use]
    pub const fn kind(mut self, kind: IconButtonKind) -> Self {
        self.kind = kind;
        self
    }

    /// Its diameter.
    #[must_use]
    pub const fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
}

impl Widget for IconButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = palette(ui.ctx());
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(self.size), Sense::click());
        let label = self.label.to_owned();
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label.clone()));
        let response = response.on_hover_text(self.label).on_hover_cursor(CursorIcon::PointingHand);
        if ui.is_rect_visible(rect) {
            let hovered = response.hovered();
            let (fill, ink) = match self.kind {
                IconButtonKind::OnBand => (Color32::from_black_alpha(if hovered { 70 } else { 46 }), p.on_accent),
                IconButtonKind::OnSheet => (if hovered { p.surface_hover } else { p.surface }, p.text),
            };
            let painter = ui.painter();
            painter.circle_filled(rect.center(), self.size / 2.0, fill);
            painter.text(rect.center(), Align2::CENTER_CENTER, self.icon, crate::fonts::icon(self.size * 0.48), ink);
        }
        response
    }
}

/// A dashed outline that adds something: "Add a local checkout".
pub struct AddButton<'a> {
    text: &'a str,
    height: f32,
}

impl<'a> AddButton<'a> {
    /// A dashed button reading `text`.
    #[must_use]
    pub const fn new(text: &'a str) -> Self {
        Self { text, height: size::ROW }
    }
}

impl Widget for AddButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = palette(ui.ctx());
        let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), self.height), Sense::click());
        let response = response.on_hover_cursor(CursorIcon::PointingHand);
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let ink = if response.hovered() { p.text } else { p.secondary };
            let r = f32::from(radius::ROW);
            let rect = rect.shrink(1.0);
            // A rounded dashed outline: straight dashed edges plus quarter
            // arcs at the corners.
            let mut outline = Vec::new();
            let corners = [
                (rect.right_top() + Vec2::new(-r, r), -90.0_f32),
                (rect.right_bottom() + Vec2::new(-r, -r), 0.0),
                (rect.left_bottom() + Vec2::new(r, -r), 90.0),
                (rect.left_top() + Vec2::new(r, r), 180.0),
            ];
            for (center, start) in corners {
                for step in 0..=6 {
                    #[allow(clippy::cast_precision_loss)]
                    let angle = (start + 15.0 * step as f32).to_radians();
                    outline.push(center + r * Vec2::angled(angle));
                }
            }
            outline.push(outline[0]);
            painter.extend(Shape::dashed_line(&outline, Stroke::new(2.0, p.outline), 7.0, 5.0));
            let label = labeled(ui, Some(crate::icons::ADD), self.text, None, &Type::Button.sized(13.5), ink);
            painter.galley(rect.center() - label.size() / 2.0, label, ink);
        }
        response
    }
}
