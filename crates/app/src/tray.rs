//! The tray item: its menu, and an icon coloured by overall health.

use fastframe_tray::{Config, DrawIcon, MenuItem, Tray};
use signoff_core::Health;

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
            // Health is shown by colour, so no monochrome template yet; the
            // design system adds menu-bar template variants per state.
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
    ring(size, [0x8e, 0x8e, 0x93])
}
fn ready(size: usize) -> Vec<u8> {
    ring(size, [0x30, 0xa4, 0x6c])
}
fn attention(size: usize) -> Vec<u8> {
    ring(size, [0xe0, 0x9b, 0x1a])
}
fn failing(size: usize) -> Vec<u8> {
    ring(size, [0xd9, 0x3f, 0x3f])
}

/// A placeholder mark: a ring with a filled centre, anti-aliased, as RGBA.
fn ring(size: usize, rgb: [u8; 3]) -> Vec<u8> {
    let mut pixels = vec![0u8; size * size * 4];
    let centre = size as f32 / 2.0;
    let outer = centre - 0.5;
    let stroke = (size as f32 / 9.0).max(1.5);
    let dot = outer * 0.42;
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 + 0.5 - centre).powi(2) + (y as f32 + 0.5 - centre).powi(2)).sqrt();
            let ring = (outer - d + 0.5).clamp(0.0, 1.0) * (d - (outer - stroke) + 0.5).clamp(0.0, 1.0);
            let fill = (dot - d + 0.5).clamp(0.0, 1.0);
            let alpha = ring.max(fill);
            let at = (y * size + x) * 4;
            pixels[at..at + 3].copy_from_slice(&rgb);
            pixels[at + 3] = (alpha * 255.0).round() as u8;
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_are_rgba_and_transparent_at_the_corners() {
        for health in [Health::Neutral, Health::Ready, Health::Attention, Health::Failing] {
            let pixels = icon(health)(32);
            assert_eq!(pixels.len(), 32 * 32 * 4);
            assert_eq!(pixels[3], 0, "corner is transparent");
            let centre = (16 * 32 + 16) * 4;
            assert_eq!(pixels[centre + 3], 255, "centre is opaque");
        }
    }
}
