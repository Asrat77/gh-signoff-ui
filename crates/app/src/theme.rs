//! A placeholder theme: the few tokens the shell needs, in light and dark.
//!
//! The design system replaces this with the real palette, type scale and
//! widgets. Everything the views draw takes its colours from [`Palette`],
//! so swapping it is one module. It implements [`fastframe_theme::Palette`]
//! so palette files and light/dark pairs work from the start.

use egui::{Color32, Theme};
use signoff_core::Health;

/// Colour tokens.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Palette {
    pub(crate) dark: bool,
    pub(crate) window: Color32,
    pub(crate) surface: Color32,
    pub(crate) text: Color32,
    pub(crate) muted: Color32,
    pub(crate) accent: Color32,
    pub(crate) ready: Color32,
    pub(crate) attention: Color32,
    pub(crate) failing: Color32,
    pub(crate) neutral: Color32,
}

impl Palette {
    /// The colour for a health state.
    pub(crate) fn health(&self, health: Health) -> Color32 {
        match health {
            Health::Neutral => self.neutral,
            Health::Ready => self.ready,
            Health::Attention => self.attention,
            Health::Failing => self.failing,
        }
    }

    /// The palette for the theme `ctx` shows now.
    pub(crate) fn current(ctx: &egui::Context) -> Self {
        <Self as fastframe_theme::Palette>::base(fastframe_theme::Base::from_dark(ctx.theme() == Theme::Dark))
    }

    fn visuals(&self) -> egui::Visuals {
        let mut visuals = if self.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        visuals.panel_fill = self.window;
        visuals.window_fill = self.surface;
        visuals.extreme_bg_color = self.surface;
        visuals.override_text_color = Some(self.text);
        visuals.hyperlink_color = self.accent;
        visuals.selection.bg_fill = self.accent;
        visuals
    }
}

impl fastframe_theme::Palette for Palette {
    fn base(base: fastframe_theme::Base) -> Self {
        let hex = Color32::from_hex;
        let c = |value: &str| hex(value).unwrap_or(Color32::MAGENTA);
        match base {
            fastframe_theme::Base::Dark => Self {
                dark: true,
                window: c("#1c1c1e"),
                surface: c("#2c2c2e"),
                text: c("#f2f2f7"),
                muted: c("#98989f"),
                accent: c("#5e9eff"),
                ready: c("#3ccf8e"),
                attention: c("#f5b43c"),
                failing: c("#ff6961"),
                neutral: c("#8e8e93"),
            },
            fastframe_theme::Base::Light => Self {
                dark: false,
                window: c("#f5f5f7"),
                surface: c("#ffffff"),
                text: c("#1c1c1e"),
                muted: c("#6e6e73"),
                accent: c("#0a66d8"),
                ready: c("#1f8a57"),
                attention: c("#b26b00"),
                failing: c("#c9352f"),
                neutral: c("#8e8e93"),
            },
        }
    }

    fn set(&mut self, name: &str, color: Color32) -> bool {
        let slot = match name {
            "window" => &mut self.window,
            "surface" => &mut self.surface,
            "text" => &mut self.text,
            "muted" => &mut self.muted,
            "accent" => &mut self.accent,
            "ready" => &mut self.ready,
            "attention" => &mut self.attention,
            "failing" => &mut self.failing,
            "neutral" => &mut self.neutral,
            _ => return false,
        };
        *slot = color;
        true
    }
}

/// Installs both palettes and follows the system's light or dark mode.
pub(crate) fn install(ctx: &egui::Context) {
    use fastframe_theme::{Base, Palette as _};
    ctx.set_visuals_of(Theme::Dark, Palette::base(Base::Dark).visuals());
    ctx.set_visuals_of(Theme::Light, Palette::base(Base::Light).visuals());
    ctx.set_theme(egui::ThemePreference::System);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_files_can_override_tokens() {
        let palette: Palette =
            fastframe_theme::parse_palette(r##"{"base":"light","colors":{"accent":"#102030"}}"##).expect("palette");
        assert!(!palette.dark);
        assert_eq!(palette.accent, Color32::from_rgb(0x10, 0x20, 0x30));
        assert!(fastframe_theme::parse_palette::<Palette>(r##"{"colors":{"nope":"#000000"}}"##).is_err());
    }
}
