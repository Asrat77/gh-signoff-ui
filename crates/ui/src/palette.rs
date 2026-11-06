//! Signoff's colours.
//!
//! A black ground, raised rows, and one loud ember. Ember means "act here":
//! the band across the top, a failing pull request, the step that needs
//! you. Status colours are only ever used for state, and every state also
//! has its own glyph ([`crate::Status`]), so nothing depends on colour
//! alone.
//!
//! [`Palette`] implements [`fastframe_theme::Palette`], so palette files can
//! restyle it by name and a [`fastframe_theme::Pair`] can hold a light and
//! a dark one.

use std::collections::BTreeSet;

use egui::Color32;
use fastframe_theme::Base;

/// Every colour the interface draws with.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    /// Whether this is a dark palette.
    pub dark: bool,
    /// The ground behind everything.
    pub window: Color32,
    /// The terminal and other sunken panels.
    pub panel: Color32,
    /// A row, card or chip.
    pub surface: Color32,
    /// A row under the pointer.
    pub surface_hover: Color32,
    /// An icon tile or input inside a row, and a pressed row.
    pub surface_active: Color32,
    /// Dividers, dashed outlines and empty progress segments.
    pub outline: Color32,
    /// Primary text.
    pub text: Color32,
    /// Secondary text: the second line of a row.
    pub secondary: Color32,
    /// Placeholders and the quietest text.
    pub dim: Color32,
    /// The band across the top and other fills that carry no text.
    pub accent: Color32,
    /// The accent under the pointer.
    pub accent_hover: Color32,
    /// Text and icons on the accent.
    pub on_accent: Color32,
    /// A failing row or check, filled. Darker than the accent so white
    /// text on it stays readable.
    pub danger: Color32,
    /// A signoff that is waiting on you.
    pub warning: Color32,
    /// Every required signoff passes.
    pub success: Color32,
    /// A local command is running.
    pub running: Color32,
    /// The main button: white on dark, black on light.
    pub primary: Color32,
    /// Text on [`Self::primary`].
    pub on_primary: Color32,
    /// Command output text.
    pub terminal_text: Color32,
    /// The dimmer behind a modal.
    pub overlay: Color32,
    /// Drop shadows.
    pub shadow: Color32,
}

const fn hex(rgb: u32) -> Color32 {
    let [_, r, g, b] = rgb.to_be_bytes();
    Color32::from_rgb(r, g, b)
}

impl Palette {
    /// The dark palette, Signoff's default look.
    #[must_use]
    pub fn dark() -> Self {
        Self {
            dark: true,
            window: hex(0x0B0B0C),
            panel: hex(0x141416),
            surface: hex(0x1C1C1F),
            surface_hover: hex(0x232327),
            surface_active: hex(0x2A2A2E),
            outline: hex(0x2E2E33),
            text: hex(0xFFFFFF),
            secondary: hex(0x9C9CA3),
            dim: hex(0x909098),
            accent: hex(0xFF4A1A),
            accent_hover: hex(0xFF6236),
            on_accent: hex(0xFFFFFF),
            danger: hex(0xDB3A0F),
            warning: hex(0xFFB224),
            success: hex(0x4ADE80),
            running: hex(0x7CC8FF),
            primary: hex(0xFFFFFF),
            on_primary: hex(0x0B0B0C),
            terminal_text: hex(0xD6D6DA),
            overlay: Color32::from_black_alpha(140),
            shadow: Color32::from_black_alpha(180),
        }
    }

    /// The light palette, for people whose system is set to light.
    #[must_use]
    pub fn light() -> Self {
        Self {
            dark: false,
            window: hex(0xF3F2EF),
            panel: hex(0x141416),
            surface: hex(0xFFFFFF),
            surface_hover: hex(0xF7F6F3),
            surface_active: hex(0xF0EFEC),
            outline: hex(0xDEDDD9),
            text: hex(0x0B0B0C),
            secondary: hex(0x5E5E66),
            dim: hex(0x66666E),
            accent: hex(0xFF4A1A),
            accent_hover: hex(0xF03E10),
            on_accent: hex(0xFFFFFF),
            danger: hex(0xDB3A0F),
            warning: hex(0x955600),
            success: hex(0x13733A),
            running: hex(0x1F6FB8),
            primary: hex(0x0B0B0C),
            on_primary: hex(0xFFFFFF),
            terminal_text: hex(0xD6D6DA),
            overlay: Color32::from_black_alpha(90),
            shadow: Color32::from_black_alpha(60),
        }
    }

    /// Every colour by its palette-file name, in a fixed order.
    #[must_use]
    pub fn named(&self) -> [(&'static str, Color32); 21] {
        [
            ("window", self.window),
            ("panel", self.panel),
            ("surface", self.surface),
            ("surface_hover", self.surface_hover),
            ("surface_active", self.surface_active),
            ("outline", self.outline),
            ("text", self.text),
            ("secondary", self.secondary),
            ("dim", self.dim),
            ("accent", self.accent),
            ("accent_hover", self.accent_hover),
            ("on_accent", self.on_accent),
            ("danger", self.danger),
            ("warning", self.warning),
            ("success", self.success),
            ("running", self.running),
            ("primary", self.primary),
            ("on_primary", self.on_primary),
            ("terminal_text", self.terminal_text),
            ("overlay", self.overlay),
            ("shadow", self.shadow),
        ]
    }

    fn slot(&mut self, name: &str) -> Option<&mut Color32> {
        Some(match name {
            "window" => &mut self.window,
            "panel" => &mut self.panel,
            "surface" => &mut self.surface,
            "surface_hover" => &mut self.surface_hover,
            "surface_active" => &mut self.surface_active,
            "outline" => &mut self.outline,
            "text" => &mut self.text,
            "secondary" => &mut self.secondary,
            "dim" => &mut self.dim,
            "accent" => &mut self.accent,
            "accent_hover" => &mut self.accent_hover,
            "on_accent" => &mut self.on_accent,
            "danger" => &mut self.danger,
            "warning" => &mut self.warning,
            "success" => &mut self.success,
            "running" => &mut self.running,
            "primary" => &mut self.primary,
            "on_primary" => &mut self.on_primary,
            "terminal_text" => &mut self.terminal_text,
            "overlay" => &mut self.overlay,
            "shadow" => &mut self.shadow,
            _ => return None,
        })
    }

    /// `color` laid over `under` at `amount` (0 to 1), for tints such as a
    /// status colour behind its glyph.
    #[must_use]
    pub fn mix(under: Color32, color: Color32, amount: f32) -> Color32 {
        let amount = amount.clamp(0.0, 1.0);
        let channel = |a: u8, b: u8| {
            let value = f32::from(a) + (f32::from(b) - f32::from(a)) * amount;
            // Rounded and clamped to 0..=255 above, so the cast is exact.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let value = value.round().clamp(0.0, 255.0) as u8;
            value
        };
        Color32::from_rgb(channel(under.r(), color.r()), channel(under.g(), color.g()), channel(under.b(), color.b()))
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::dark()
    }
}

impl fastframe_theme::Palette for Palette {
    fn base(base: Base) -> Self {
        match base {
            Base::Dark => Self::dark(),
            Base::Light => Self::light(),
        }
    }

    fn set(&mut self, name: &str, color: Color32) -> bool {
        self.slot(name).map(|slot| *slot = color).is_some()
    }

    /// A file that only sets the shared base colours still gets a matching
    /// main button and failing fill.
    fn derive(&mut self, given: &BTreeSet<&str>) {
        if given.contains("accent") && !given.contains("accent_hover") {
            self.accent_hover = Self::mix(self.accent, self.text, 0.12);
        }
        if given.contains("text") && !given.contains("primary") {
            self.primary = self.text;
        }
        if given.contains("window") && !given.contains("on_primary") {
            self.on_primary = self.window;
        }
    }
}

impl fastframe_theme::PaletteBase for Palette {
    fn palette_base(&self) -> Base {
        Base::from_dark(self.dark)
    }
}

/// The WCAG 2 contrast ratio between two opaque colours, from 1 to 21.
#[must_use]
pub fn contrast(a: Color32, b: Color32) -> f32 {
    fn luminance(color: Color32) -> f32 {
        let linear = |channel: u8| {
            let c = f32::from(channel) / 255.0;
            if c <= 0.039_28 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
    }
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

#[cfg(test)]
mod tests {
    use fastframe_theme::{BASE_COLORS, Palette as _, parse_palette};

    use super::*;

    const AA: f32 = 4.5;

    fn both() -> [Palette; 2] {
        [Palette::dark(), Palette::light()]
    }

    #[test]
    fn text_is_readable_where_it_is_drawn() {
        for p in both() {
            let grounds = [p.window, p.surface, p.surface_active];
            for ground in grounds {
                for (name, ink) in [("text", p.text), ("secondary", p.secondary), ("dim", p.dim)] {
                    let ratio = contrast(ink, ground);
                    assert!(ratio >= AA, "{name} on {ground:?}: {ratio:.2} (dark: {})", p.dark);
                }
            }
            assert!(contrast(p.on_accent, p.danger) >= AA, "white on a failing row");
            assert!(contrast(p.on_primary, p.primary) >= AA, "the main button");
            assert!(contrast(p.terminal_text, p.panel) >= AA, "command output");
        }
    }

    #[test]
    fn status_colours_are_readable_on_icon_tiles() {
        for p in both() {
            for (name, color) in [("warning", p.warning), ("success", p.success), ("running", p.running)] {
                let ratio = contrast(color, p.surface_active);
                assert!(ratio >= AA, "{name}: {ratio:.2} (dark: {})", p.dark);
            }
        }
    }

    #[test]
    fn every_shared_colour_name_is_understood() {
        for name in BASE_COLORS {
            assert!(Palette::dark().slot(name).is_some(), "{name}");
        }
    }

    #[test]
    fn named_lists_every_slot() {
        let mut palette = Palette::dark();
        for (name, color) in Palette::dark().named() {
            assert_eq!(palette.slot(name).copied(), Some(color), "{name}");
        }
    }

    #[test]
    fn palette_files_restyle_by_name() {
        let palette: Palette = parse_palette(r##"{"base":"light","colors":{"accent":"#3366ff","text":"#101010"}}"##)
            .expect("a valid file");
        assert!(!palette.dark);
        assert_eq!(palette.accent, hex(0x3366FF));
        assert_eq!(palette.primary, hex(0x101010), "the button follows the text");
        assert!(parse_palette::<Palette>(r##"{"colors":{"nope":"#000000"}}"##).is_err());
    }

    #[test]
    fn bases_match_their_constructors() {
        assert_eq!(Palette::base(Base::Dark), Palette::dark());
        assert_eq!(Palette::base(Base::Light), Palette::light());
    }

    #[test]
    fn mix_runs_from_one_colour_to_the_other() {
        let (a, b) = (Color32::BLACK, Color32::WHITE);
        assert_eq!(Palette::mix(a, b, 0.0), a);
        assert_eq!(Palette::mix(a, b, 1.0), b);
        assert_eq!(Palette::mix(a, b, 0.5), Color32::from_rgb(128, 128, 128));
    }
}
