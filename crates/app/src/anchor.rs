//! Where the popover window opens: by the tray, in the screen corner the
//! platform keeps it in.
//!
//! No tray library reports where its icon is drawn, so the popover goes to
//! the corner the tray lives in by convention: top right under the macOS
//! menu bar and most Linux panels, bottom right above the Windows taskbar.
//! Wayland compositors place windows themselves and ignore this; the band
//! can be dragged everywhere.

use egui::{Pos2, Vec2};

/// The popover's size, in points.
pub(crate) const POPOVER: Vec2 = Vec2::new(400.0, 660.0);

/// Space between the popover and the screen's edge.
const MARGIN: f32 = 10.0;
/// Room for the macOS menu bar or a top panel.
const TOP_BAR: f32 = 30.0;
/// Room for the Windows taskbar.
const TASKBAR: f32 = 52.0;

/// A screen corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Corner {
    /// Under a top bar.
    TopRight,
    /// Above a taskbar.
    BottomRight,
}

impl Corner {
    /// The corner the tray sits in on this platform.
    pub(crate) const fn of_tray() -> Self {
        if cfg!(windows) { Self::BottomRight } else { Self::TopRight }
    }
}

/// The window's top-left corner for a popover of `size` in `corner` of a
/// monitor of `monitor` points. A popover taller or wider than the monitor
/// keeps its top-left corner on screen.
pub(crate) fn anchor(monitor: Vec2, size: Vec2, corner: Corner) -> Pos2 {
    let x = monitor.x - size.x - MARGIN;
    let y = match corner {
        Corner::TopRight => TOP_BAR + MARGIN,
        Corner::BottomRight => monitor.y - size.y - TASKBAR - MARGIN,
    };
    Pos2::new(x.max(0.0), y.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Vec2 = Vec2::new(1440.0, 900.0);

    #[test]
    fn top_right_sits_under_the_bar_by_the_right_edge() {
        let at = anchor(SCREEN, POPOVER, Corner::TopRight);
        assert_eq!(at.x + POPOVER.x + MARGIN, SCREEN.x);
        assert_eq!(at.y, TOP_BAR + MARGIN);
    }

    #[test]
    fn bottom_right_sits_above_the_taskbar() {
        let at = anchor(SCREEN, POPOVER, Corner::BottomRight);
        assert_eq!(at.y + POPOVER.y + MARGIN + TASKBAR, SCREEN.y);
    }

    #[test]
    fn a_small_screen_keeps_the_corner_on_screen() {
        let at = anchor(Vec2::new(320.0, 480.0), POPOVER, Corner::BottomRight);
        assert_eq!(at, Pos2::ZERO);
    }
}
