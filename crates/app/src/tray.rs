//! The tray item: its menu, and the eyes coloured by overall health.

use egui::Color32;
use fastframe_tray::{Config, DrawIcon, MenuItem, Tray};
use signoff_core::Health;
use signoff_ui::widgets::Mood;
use signoff_ui::{Palette, Status};

/// Menu entry ids.
pub(crate) const OPEN: &str = "open";
pub(crate) const REFRESH: &str = "refresh";
pub(crate) const SETTINGS: &str = "settings";
pub(crate) const QUIT: &str = "quit";

/// Creates the tray item, or `None` where there is no tray at all.
pub(crate) fn spawn(wake: impl Fn() + Send + Sync + 'static) -> Option<Tray> {
    Tray::spawn(
        Config {
            id: "signoff",
            title: "Signoff".into(),
            icon: icon(Health::Neutral),
            // Health is shown by colour, so the macOS menu bar gets the
            // coloured icon too rather than a monochrome template.
            template_icon: None,
            themed_icon: false,
            menu: vec![
                MenuItem::action(OPEN, "Open Signoff"),
                MenuItem::action(REFRESH, "Refresh now"),
                MenuItem::action(SETTINGS, "Settings…"),
                MenuItem::Separator,
                MenuItem::action(QUIT, "Quit Signoff"),
            ],
        },
        wake,
    )
}

/// The icon for `health`.
pub(crate) fn icon(health: Health) -> DrawIcon {
    match health {
        Health::Neutral => neutral,
        Health::Ready => ready,
        Health::Attention => attention,
        Health::Failing => failing,
    }
}

/// The tooltip for a summary.
pub(crate) fn tooltip(summary: &signoff_core::ProductSummary, demo: bool) -> String {
    let mut text = String::from("Signoff");
    if demo {
        text.push_str(" (demo)");
    }
    text.push('\n');
    text.push_str(&match summary.total_pull_requests {
        0 => "No open pull requests".to_owned(),
        total => format!("{} ready · {} need attention · {total} open", summary.ready, summary.needs_attention),
    });
    text
}

fn neutral(size: usize) -> Vec<u8> {
    eyes(size, Status::Neutral)
}
fn ready(size: usize) -> Vec<u8> {
    eyes(size, Status::Ready)
}
fn attention(size: usize) -> Vec<u8> {
    eyes(size, Status::Attention)
}
fn failing(size: usize) -> Vec<u8> {
    eyes(size, Status::Failing)
}

/// Signoff's mark at tray size: the eyes on a disk in the state's colour,
/// with the pupils looking where the band's eyes look. Anti-aliased RGBA.
fn eyes(size: usize, status: Status) -> Vec<u8> {
    let palette = Palette::dark();
    let disk = match status {
        Status::Failing => palette.danger,
        Status::Attention => palette.warning,
        Status::Ready => palette.success,
        Status::Running => palette.running,
        // Light grey reads on both dark and light panels.
        Status::Neutral => Palette::light().secondary,
    };
    let white = Color32::WHITE;
    let ink = Color32::from_rgb(0x11, 0x11, 0x11);
    let gaze = Mood::for_status(status).gaze();

    #[allow(clippy::cast_precision_loss)]
    let s = size as f32;
    let centre = s / 2.0;
    let eye = (s * 0.17, s * 0.25);
    let pupil = (eye.0 * 0.5, eye.1 * 0.4);
    let travel = (eye.0 - pupil.0 - s * 0.03, eye.1 - pupil.1 - s * 0.03);
    let eye_centres = [centre - s * 0.19, centre + s * 0.19];

    let mut pixels = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            #[allow(clippy::cast_precision_loss)]
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let disk_cover = ellipse(px - centre, py - centre, (centre - 0.5, centre - 0.5));
            let mut colour = disk;
            for ex in eye_centres {
                let (dx, dy) = (px - ex, py - centre);
                colour = blend(colour, white, ellipse(dx, dy, eye));
                let (gx, gy) = (gaze.x * travel.0, gaze.y * travel.1);
                colour = blend(colour, ink, ellipse(dx - gx, dy - gy, pupil));
            }
            let at = (y * size + x) * 4;
            pixels[at..at + 3].copy_from_slice(&[colour.r(), colour.g(), colour.b()]);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let alpha = (disk_cover * 255.0).round() as u8;
            pixels[at + 3] = alpha;
        }
    }
    pixels
}

/// How much of the pixel at `(dx, dy)` from an ellipse's centre it covers,
/// from 0 to 1, with a one-pixel soft edge.
fn ellipse(dx: f32, dy: f32, (rx, ry): (f32, f32)) -> f32 {
    let d = ((dx / rx).powi(2) + (dy / ry).powi(2)).sqrt();
    ((1.0 - d) * rx.min(ry) + 0.5).clamp(0.0, 1.0)
}

/// `top` over `bottom`, `amount` of the way.
fn blend(bottom: Color32, top: Color32, amount: f32) -> Color32 {
    let mix = |a: u8, b: u8| {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let value = (f32::from(a) + (f32::from(b) - f32::from(a)) * amount).round() as u8;
        value
    };
    Color32::from_rgb(mix(bottom.r(), top.r()), mix(bottom.g(), top.g()), mix(bottom.b(), top.b()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(pixels: &[u8], size: usize, x: usize, y: usize) -> [u8; 4] {
        let at = (y * size + x) * 4;
        [pixels[at], pixels[at + 1], pixels[at + 2], pixels[at + 3]]
    }

    #[test]
    fn icons_are_rgba_and_transparent_at_the_corners() {
        for health in [Health::Neutral, Health::Ready, Health::Attention, Health::Failing] {
            for size in [16, 22, 32, 64] {
                let pixels = icon(health)(size);
                assert_eq!(pixels.len(), size * size * 4);
                assert_eq!(pixel(&pixels, size, 0, 0)[3], 0, "corner is transparent");
                assert_eq!(pixel(&pixels, size, size / 2, size / 2)[3], 255, "centre is opaque");
            }
        }
    }

    #[test]
    fn the_disk_takes_the_state_colour() {
        let palette = Palette::dark();
        let size = 64;
        // Just inside the left edge, clear of the eyes.
        let edge = |health| pixel(&icon(health)(size), size, 4, size / 2);
        assert_eq!(edge(Health::Failing)[..3], [palette.danger.r(), palette.danger.g(), palette.danger.b()]);
        assert_eq!(edge(Health::Ready)[..3], [palette.success.r(), palette.success.g(), palette.success.b()]);
        assert_ne!(edge(Health::Attention), edge(Health::Neutral));
    }

    #[test]
    fn worried_eyes_look_down() {
        let size = 64;
        let failing = icon(Health::Failing)(size);
        let ready = icon(Health::Ready)(size);
        // The left eye's lowest white pixel row is covered by the pupil
        // when worried, and free when relaxed.
        let dark = |pixels: &[u8], y: usize| pixel(pixels, size, 19, y)[0] < 0x40;
        assert!(dark(&failing, 40));
        assert!(!dark(&ready, 40));
    }
}
