//! Spacing, shape and motion tokens, and installing the theme into egui.
//!
//! Call [`install`] once when the app starts. Widgets then read the
//! current colours with [`palette`], which follows the system's light or
//! dark appearance unless the user picked one.

use std::sync::Arc;

use egui::{
    Color32, Context, CornerRadius, FontFamily, Id, Margin, Shadow, Stroke, Style, TextStyle, Theme, ThemePreference,
    Vec2, Visuals,
};

use crate::{Palette, Type, fonts};

/// The spacing scale, in points.
pub mod space {
    /// Between an icon and its label.
    pub const XS: f32 = 4.0;
    /// Between rows, chips and segments.
    pub const S: f32 = 8.0;
    /// Inside a row.
    pub const M: f32 = 12.0;
    /// Around a group of rows.
    pub const L: f32 = 16.0;
    /// Around a screen's content.
    pub const XL: f32 = 22.0;
    /// Between sections.
    pub const XXL: f32 = 32.0;
}

/// Corner radii, in points.
pub mod radius {
    /// Icon tiles and small buttons.
    pub const TILE: u8 = 12;
    /// Text inputs.
    pub const INPUT: u8 = 14;
    /// Buttons.
    pub const BUTTON: u8 = 16;
    /// Rows and cards.
    pub const ROW: u8 = 18;
    /// The sheet under the band, and the popover itself.
    pub const SHEET: u8 = 26;
    /// Chips, pills and progress segments: fully round.
    pub const PILL: u8 = u8::MAX;
}

/// Heights of the controls, in points.
pub mod size {
    /// A row with two lines of text.
    pub const ROW: f32 = 64.0;
    /// An icon tile inside a row.
    pub const TILE: f32 = 38.0;
    /// The main button.
    pub const BUTTON: f32 = 48.0;
    /// A chip or a nav pill.
    pub const CHIP: f32 = 34.0;
    /// A round icon button.
    pub const ICON_BUTTON: f32 = 32.0;
    /// A progress segment's thickness.
    pub const SEGMENT: f32 = 8.0;
    /// The band across the top of the popover.
    pub const BAND: f32 = 64.0;
}

/// How long things move, in seconds.
pub mod motion {
    /// Hover, press and expand.
    pub const FAST: f32 = 0.16;
    /// Progress filling and the eyes looking around.
    pub const GLIDE: f32 = 0.3;
    /// One turn of the running spinner.
    pub const SPIN: f32 = 0.9;
}

/// Which appearance to show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ThemeMode {
    /// Follow the system's light or dark setting.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

impl From<ThemeMode> for ThemePreference {
    fn from(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::System => Self::System,
            ThemeMode::Light => Self::Light,
            ThemeMode::Dark => Self::Dark,
        }
    }
}

#[derive(Clone)]
struct Installed {
    light: Arc<Palette>,
    dark: Arc<Palette>,
    reduced_motion: bool,
}

fn key() -> Id {
    Id::new("signoff-ui-theme")
}

/// Sets up fonts, both palettes and egui's styles, and picks `mode`.
pub fn install(ctx: &Context, mode: ThemeMode) {
    ctx.set_fonts(fonts::definitions());
    set_palettes(ctx, Palette::light(), Palette::dark());
    ctx.set_theme(mode);
}

/// Replaces the light and dark palettes, for example with ones read from
/// palette files by `fastframe_theme`.
pub fn set_palettes(ctx: &Context, light: Palette, dark: Palette) {
    let reduced_motion = reduced_motion(ctx);
    ctx.set_style_of(Theme::Light, style(&light, reduced_motion));
    ctx.set_style_of(Theme::Dark, style(&dark, reduced_motion));
    ctx.data_mut(|data| {
        data.insert_temp(key(), Installed { light: Arc::new(light), dark: Arc::new(dark), reduced_motion });
    });
}

/// The palette for the appearance egui is showing now.
#[must_use]
pub fn palette(ctx: &Context) -> Arc<Palette> {
    let dark = ctx.theme() == Theme::Dark;
    ctx.data(|data| data.get_temp::<Installed>(key()))
        .map(|installed| if dark { installed.dark } else { installed.light })
        .unwrap_or_else(|| Arc::new(if dark { Palette::dark() } else { Palette::light() }))
}

/// Turns animation off or on. Pass the operating system's "reduce
/// motion" setting, or the user's own.
pub fn set_reduced_motion(ctx: &Context, reduced: bool) {
    ctx.all_styles_mut(|style| style.animation_time = animation_time(reduced));
    ctx.data_mut(|data| {
        data.get_temp_mut_or_insert_with(key(), || Installed {
            light: Arc::new(Palette::light()),
            dark: Arc::new(Palette::dark()),
            reduced_motion: reduced,
        })
        .reduced_motion = reduced;
    });
}

/// Whether animation is off.
#[must_use]
pub fn reduced_motion(ctx: &Context) -> bool {
    ctx.data(|data| data.get_temp::<Installed>(key())).is_some_and(|installed| installed.reduced_motion)
}

/// Seconds to animate for: `seconds`, or none when motion is reduced.
#[must_use]
pub fn duration(ctx: &Context, seconds: f32) -> f32 {
    if reduced_motion(ctx) { 0.0 } else { seconds }
}

fn animation_time(reduced: bool) -> f32 {
    if reduced { 0.0 } else { motion::FAST }
}

/// egui's style for `palette`: text styles, spacing and visuals.
#[must_use]
pub fn style(palette: &Palette, reduced_motion: bool) -> Style {
    let mut style =
        Style { visuals: visuals(palette), animation_time: animation_time(reduced_motion), ..Style::default() };
    style.text_styles = [
        (TextStyle::Heading, Type::Heading.font()),
        (TextStyle::Body, Type::Body.font()),
        (TextStyle::Button, Type::Button.font()),
        (TextStyle::Small, Type::Caption.font()),
        (TextStyle::Monospace, Type::Mono.font()),
    ]
    .into();
    let spacing = &mut style.spacing;
    spacing.item_spacing = Vec2::new(space::S, space::S);
    spacing.button_padding = Vec2::new(space::L, space::S);
    spacing.window_margin = Margin::same(16);
    spacing.menu_margin = Margin::same(8);
    spacing.interact_size = Vec2::new(40.0, 32.0);
    spacing.icon_spacing = space::S;
    style
}

/// egui's visuals for `palette`, so stock widgets match ours.
#[must_use]
pub fn visuals(p: &Palette) -> Visuals {
    let mut v = if p.dark { Visuals::dark() } else { Visuals::light() };
    v.override_text_color = Some(p.text);
    v.weak_text_color = Some(p.secondary);
    v.hyperlink_color = p.text;
    v.faint_bg_color = p.surface;
    v.extreme_bg_color = p.surface_active;
    v.text_edit_bg_color = Some(p.surface_active);
    v.code_bg_color = p.panel;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.window_fill = p.window;
    v.panel_fill = p.window;
    v.window_stroke = Stroke::NONE;
    v.window_corner_radius = CornerRadius::same(radius::ROW);
    v.menu_corner_radius = CornerRadius::same(radius::TILE);
    let shadow = Shadow { offset: [0, 16], blur: 40, spread: 0, color: p.shadow };
    v.window_shadow = shadow;
    v.popup_shadow = shadow;
    v.selection.bg_fill = Palette::mix(p.surface, p.accent, 0.35);
    v.selection.stroke = Stroke::new(1.0, p.text);

    let rounded = CornerRadius::same(radius::TILE);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.window;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.outline);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = rounded;
    for (state, fill) in [
        (&mut w.inactive, p.surface),
        (&mut w.hovered, p.surface_hover),
        (&mut w.active, p.surface_active),
        (&mut w.open, p.surface_active),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::NONE;
        state.fg_stroke = Stroke::new(1.5, p.text);
        state.corner_radius = rounded;
        state.expansion = 0.0;
    }
    w.hovered.bg_stroke = Stroke::new(1.0, p.outline);
    v.disabled_alpha = 0.45;
    v
}

/// A colour with `alpha` (0 to 1) applied, for hover tints and dimmed
/// text.
#[must_use]
pub fn faded(color: Color32, alpha: f32) -> Color32 {
    color.gamma_multiply(alpha)
}

/// The family used for filled icons, re-exported for widgets.
#[must_use]
pub fn icon_fill_family() -> FontFamily {
    fonts::icon_fill_family()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_follows_the_chosen_appearance() {
        let ctx = Context::default();
        install(&ctx, ThemeMode::Dark);
        assert!(palette(&ctx).dark);
        ctx.set_theme(ThemeMode::Light);
        assert!(!palette(&ctx).dark);
    }

    #[test]
    fn custom_palettes_replace_the_defaults() {
        let ctx = Context::default();
        install(&ctx, ThemeMode::Dark);
        let mut dark = Palette::dark();
        dark.accent = Color32::from_rgb(1, 2, 3);
        set_palettes(&ctx, Palette::light(), dark.clone());
        assert_eq!(*palette(&ctx), dark);
        assert_eq!(ctx.style_of(Theme::Dark).visuals.window_fill, dark.window);
    }

    #[test]
    fn reduced_motion_stops_animation() {
        let ctx = Context::default();
        install(&ctx, ThemeMode::Dark);
        assert!(duration(&ctx, motion::GLIDE) > 0.0);
        set_reduced_motion(&ctx, true);
        assert!(reduced_motion(&ctx));
        assert_eq!(duration(&ctx, motion::GLIDE), 0.0);
        assert_eq!(ctx.style_of(Theme::Dark).animation_time, 0.0);
        set_palettes(&ctx, Palette::light(), Palette::dark());
        assert!(reduced_motion(&ctx), "survives a palette change");
    }

    #[test]
    fn stock_text_styles_use_the_type_scale() {
        let style = style(&Palette::dark(), false);
        assert_eq!(style.text_styles[&TextStyle::Monospace], Type::Mono.font());
        assert_eq!(style.text_styles[&TextStyle::Body], Type::Body.font());
    }
}
