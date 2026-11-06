//! Signoff's design system for egui.
//!
//! The popover, the settings window and the widget gallery all draw with
//! this crate, so the app looks the same on macOS, Windows and Linux.

pub mod ansi;
pub mod fonts;
pub mod icons;
pub mod palette;
pub mod status;

pub use fonts::Type;
pub use palette::Palette;
pub use status::Status;
pub mod theme;

pub use theme::{ThemeMode, install, palette};
pub mod widgets;
