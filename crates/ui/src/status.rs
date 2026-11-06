//! The states a pull request or a signoff check can be in.

use egui::Color32;

use crate::{Palette, icons};

/// Where a pull request or a check stands.
///
/// Pull requests sort by this order: failing first, then the ones waiting
/// on you, then running, then ready.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    /// A required signoff failed.
    Failing,
    /// A required signoff is waiting on you.
    Attention,
    /// A local command is running.
    Running,
    /// Every required signoff passes.
    Ready,
    /// Nothing to report: no signoffs required, or not loaded yet.
    #[default]
    Neutral,
}

impl Status {
    /// Every state, in sort order.
    pub const ALL: [Self; 5] = [Self::Failing, Self::Attention, Self::Running, Self::Ready, Self::Neutral];

    /// The glyph that marks this state, so it reads without colour.
    #[must_use]
    pub const fn icon(self) -> &'static str {
        match self {
            Self::Failing => icons::FAIL,
            Self::Attention => icons::ATTENTION,
            Self::Running => icons::RUNNING,
            Self::Ready => icons::CHECK,
            Self::Neutral => icons::PENDING,
        }
    }

    /// A short label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Failing => "Failing",
            Self::Attention => "Needs you",
            Self::Running => "Running",
            Self::Ready => "Ready",
            Self::Neutral => "No signoffs",
        }
    }

    /// The colour of this state's glyph on a plain row.
    #[must_use]
    pub fn color(self, palette: &Palette) -> Color32 {
        match self {
            Self::Failing => palette.danger,
            Self::Attention => palette.warning,
            Self::Running => palette.running,
            Self::Ready => palette.success,
            Self::Neutral => palette.secondary,
        }
    }

    /// Whether a row in this state is filled with the danger colour
    /// instead of drawn as a plain row. Only failing is that loud.
    #[must_use]
    pub const fn is_loud(self) -> bool {
        matches!(self, Self::Failing)
    }

    /// The background, text and secondary text of a row in this state.
    #[must_use]
    pub fn row_colors(self, palette: &Palette) -> RowColors {
        if self.is_loud() {
            RowColors {
                fill: palette.danger,
                text: palette.on_accent,
                secondary: palette.on_accent.gamma_multiply(0.82),
                tile: Color32::from_black_alpha(50),
                glyph: palette.on_accent,
            }
        } else {
            RowColors {
                fill: palette.surface,
                text: palette.text,
                secondary: palette.secondary,
                tile: palette.surface_active,
                glyph: self.color(palette),
            }
        }
    }
}

/// The colours a row is drawn with; see [`Status::row_colors`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowColors {
    /// The row's background.
    pub fill: Color32,
    /// Its first line.
    pub text: Color32,
    /// Its second line.
    pub secondary: Color32,
    /// The tile behind its icon.
    pub tile: Color32,
    /// The icon's colour.
    pub glyph: Color32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failing_sorts_first_and_neutral_last() {
        let mut states = vec![Status::Ready, Status::Neutral, Status::Failing, Status::Attention];
        states.sort();
        assert_eq!(states, [Status::Failing, Status::Attention, Status::Ready, Status::Neutral]);
    }

    #[test]
    fn only_failing_rows_are_filled() {
        let palette = Palette::dark();
        for status in Status::ALL {
            let fill = status.row_colors(&palette).fill;
            assert_eq!(fill == palette.danger, status == Status::Failing, "{status:?}");
        }
    }

    #[test]
    fn every_state_has_its_own_glyph() {
        for (i, a) in Status::ALL.iter().enumerate() {
            for b in &Status::ALL[i + 1..] {
                assert_ne!(a.icon(), b.icon(), "{a:?} and {b:?}");
            }
        }
    }
}
