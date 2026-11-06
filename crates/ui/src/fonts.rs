//! The faces Signoff draws with and the type scale built on them.
//!
//! Inter (bundled by `fastframe-fonts`) draws the interface at four weights
//! plus a heavy display cut for headlines. JetBrains Mono draws branches,
//! commit SHAs and command output. Phosphor icons sit behind every family,
//! so an icon can be mixed into any label.

use std::sync::Arc;

use egui::epaint::text::VariationCoords;
use egui::{FontData, FontDefinitions, FontFamily, FontId};
use fastframe_fonts::{FontSetup, INTER, Monospace, Weight};

/// JetBrains Mono 2.304, regular. SIL Open Font License 1.1, shipped as
/// `fonts/JetBrainsMono-LICENSE.txt`.
pub const JETBRAINS_MONO: &[u8] = include_bytes!("../fonts/JetBrainsMono-Regular.ttf");

/// The key and family name of the heavy display cut of Inter.
pub const DISPLAY: &str = "signoff-display";

const MONO: &str = "jetbrains-mono";
const ICONS: &str = "phosphor";
const ICONS_FILL: &str = "phosphor-fill";

/// One step of the type scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Type {
    /// 30 pt, heavy display cut: the popover's headline ("3 PRs need you").
    Display,
    /// 24 pt, heavy display cut: a PR title on its detail page.
    Title,
    /// 17 pt bold: the wordmark and section headings.
    Heading,
    /// 14 pt semibold: the first line of a row.
    Row,
    /// 13 pt regular: body copy.
    Body,
    /// 13 pt semibold: buttons and chips.
    Button,
    /// 11.5 pt regular: the second line of a row, captions.
    Caption,
    /// 11 pt mono: branches, SHAs and command output.
    Mono,
}

impl Type {
    /// Every step, largest first.
    pub const ALL: [Self; 8] =
        [Self::Display, Self::Title, Self::Heading, Self::Row, Self::Body, Self::Button, Self::Caption, Self::Mono];

    /// The size in points.
    #[must_use]
    pub const fn size(self) -> f32 {
        match self {
            Self::Display => 30.0,
            Self::Title => 24.0,
            Self::Heading => 17.0,
            Self::Row => 14.0,
            Self::Body | Self::Button => 13.0,
            Self::Caption => 11.5,
            Self::Mono => 11.0,
        }
    }

    /// The family that draws this step.
    #[must_use]
    pub fn family(self) -> FontFamily {
        match self {
            Self::Display | Self::Title => FontFamily::Name(DISPLAY.into()),
            Self::Heading => Weight::Bold.family(),
            Self::Row | Self::Button => Weight::SemiBold.family(),
            Self::Body | Self::Caption => FontFamily::Proportional,
            Self::Mono => FontFamily::Monospace,
        }
    }

    /// The font for this step.
    #[must_use]
    pub fn font(self) -> FontId {
        FontId::new(self.size(), self.family())
    }

    /// The name used for this step in tests and the gallery.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Display => "display",
            Self::Title => "title",
            Self::Heading => "heading",
            Self::Row => "row",
            Self::Body => "body",
            Self::Button => "button",
            Self::Caption => "caption",
            Self::Mono => "mono",
        }
    }
}

/// Signoff's font definitions: Inter at every weight with system fallbacks
/// for other scripts, the display cut, JetBrains Mono, and Phosphor icons.
#[must_use]
pub fn definitions() -> FontDefinitions {
    let mut fonts = FontSetup::default()
        .monospace(Monospace::Font { name: MONO.into(), data: Arc::new(FontData::from_static(JETBRAINS_MONO)) })
        .definitions();

    let mut display = FontData::from_static(INTER);
    display.tweak.coords = VariationCoords::new([(b"wght", 800.0), (b"opsz", 32.0)]);
    fonts.font_data.insert(DISPLAY.into(), Arc::new(display));
    let mut behind = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    behind.retain(|name| name != fastframe_fonts::INTER_REGULAR);
    let mut family = vec![DISPLAY.to_owned()];
    family.extend(behind);
    fonts.families.insert(FontFamily::Name(DISPLAY.into()), family);

    fonts.font_data.insert(ICONS.into(), Arc::new(FontData::from_static(egui_phosphor::Variant::Regular.font_bytes())));
    fonts
        .font_data
        .insert(ICONS_FILL.into(), Arc::new(FontData::from_static(egui_phosphor::Variant::Fill.font_bytes())));
    // Second in every family: behind the text face, so letters stay Inter,
    // and ahead of the system fallbacks, so icons resolve from Phosphor.
    for keys in fonts.families.values_mut() {
        let at = keys.len().min(1);
        keys.insert(at, ICONS.into());
    }
    let mut fill = vec![ICONS_FILL.to_owned()];
    fill.extend(fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default());
    fonts.families.insert(FontFamily::Name(ICONS_FILL.into()), fill);
    fonts
}

/// The family that draws filled Phosphor icons.
#[must_use]
pub fn icon_fill_family() -> FontFamily {
    FontFamily::Name(ICONS_FILL.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_type_step_has_a_family() {
        let fonts = definitions();
        for step in Type::ALL {
            assert!(fonts.families.contains_key(&step.family()), "{} has no family", step.name());
        }
    }

    #[test]
    fn text_faces_lead_their_families() {
        let fonts = definitions();
        assert_eq!(fonts.families[&FontFamily::Monospace][0], MONO);
        assert_eq!(fonts.families[&FontFamily::Name(DISPLAY.into())][0], DISPLAY);
        for keys in fonts.families.values() {
            assert_ne!(keys.first().map(String::as_str), Some(ICONS));
        }
    }

    #[test]
    fn icons_reach_every_family() {
        let fonts = definitions();
        for (family, keys) in &fonts.families {
            assert!(keys.iter().any(|key| key == ICONS || key == ICONS_FILL), "{family:?} cannot draw icons");
        }
    }

    #[test]
    fn the_scale_gets_smaller() {
        let sizes: Vec<f32> = Type::ALL.iter().map(|step| step.size()).collect();
        assert!(sizes.windows(2).all(|pair| pair[0] >= pair[1]));
    }
}
