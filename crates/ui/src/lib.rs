//! Signoff's design system for egui.
//!
//! The popover, the settings window and the widget gallery all draw with
//! this crate, so the app looks the same on macOS, Windows and Linux.

pub mod fonts;
pub mod palette;

pub use fonts::Type;
pub use palette::Palette;
