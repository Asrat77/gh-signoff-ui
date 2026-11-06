//! The ember band across the top, the sheet under it, and headlines.

use egui::{Align, CornerRadius, Frame, InnerResponse, Layout, Margin, Response, Ui, Widget};

use super::{Eyes, Mood};
use crate::theme::{radius, size, space};
use crate::{Type, palette};

/// The ember band: the eyes, the wordmark, and room on the right for icon
/// buttons.
pub struct Band<'a> {
    mood: Mood,
    title: &'a str,
    subtitle: Option<&'a str>,
}

impl<'a> Band<'a> {
    /// A band with the `signoff` wordmark and eyes in `mood`.
    #[must_use]
    pub const fn new(mood: Mood) -> Self {
        Self { mood, title: "signoff", subtitle: None }
    }

    /// A quieter word after the wordmark, such as "settings".
    #[must_use]
    pub const fn subtitle(mut self, subtitle: &'a str) -> Self {
        self.subtitle = Some(subtitle);
        self
    }

    /// Draws the band. `trailing` lays out right to left: add the
    /// rightmost button first.
    pub fn show<R>(self, ui: &mut Ui, trailing: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
        let p = palette(ui.ctx());
        Frame::new().fill(p.accent).inner_margin(Margin::symmetric(18, 0)).show(ui, |ui| {
            ui.set_height(size::BAND);
            ui.set_width(ui.available_width());
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = space::S + 2.0;
                ui.add(Eyes::new(self.mood).size(24.0).id(ui.id().with("band-eyes")));
                ui.label(egui::RichText::new(self.title).font(Type::Heading.font()).size(19.0).color(p.on_accent));
                if let Some(subtitle) = self.subtitle {
                    ui.label(
                        egui::RichText::new(subtitle).font(Type::Button.font()).color(p.on_accent.gamma_multiply(0.85)),
                    );
                }
                ui.with_layout(Layout::right_to_left(Align::Center), trailing).inner
            })
            .inner
        })
    }
}

/// The frame of the sheet that sits under the band: the ground colour
/// with its top corners rounded over the ember.
pub fn sheet(ui: &Ui) -> Frame {
    let p = palette(ui.ctx());
    Frame::new().fill(p.window).corner_radius(CornerRadius { nw: radius::SHEET, ne: radius::SHEET, sw: 0, se: 0 })
}

/// A big headline with an optional line under it: "3 PRs need you".
pub struct Headline<'a> {
    title: &'a str,
    detail: Option<&'a str>,
    step: Type,
    centered: bool,
}

impl<'a> Headline<'a> {
    /// A display-size headline.
    #[must_use]
    pub const fn new(title: &'a str) -> Self {
        Self { title, detail: None, step: Type::Display, centered: false }
    }

    /// The line under it.
    #[must_use]
    pub const fn detail(mut self, detail: &'a str) -> Self {
        self.detail = Some(detail);
        self
    }

    /// Title size instead of display size, for a PR's own page.
    #[must_use]
    pub const fn title_size(mut self) -> Self {
        self.step = Type::Title;
        self
    }

    /// Centre it, as on the PR page.
    #[must_use]
    pub const fn centered(mut self) -> Self {
        self.centered = true;
        self
    }
}

impl Widget for Headline<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = palette(ui.ctx());
        let layout = if self.centered { Layout::top_down(Align::Center) } else { Layout::top_down(Align::Min) };
        ui.with_layout(layout, |ui| {
            ui.spacing_mut().item_spacing.y = space::XS + 2.0;
            ui.add(egui::Label::new(egui::RichText::new(self.title).font(self.step.font()).color(p.text)).wrap());
            if let Some(detail) = self.detail {
                ui.label(egui::RichText::new(detail).font(Type::Body.font()).color(p.secondary));
            }
        })
        .response
    }
}
