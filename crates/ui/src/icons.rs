//! The icons Signoff draws, by what they mean.
//!
//! They are Phosphor glyphs (MIT), drawn as text in any family set up by
//! [`crate::fonts::definitions`]. Naming them by role keeps the widgets
//! honest: a check row asks for [`TESTS`], not for a flask.

use egui_phosphor::regular as ph;

/// Signed off, passing.
pub const CHECK: &str = ph::CHECK;
/// Failed.
pub const FAIL: &str = ph::X;
/// Waiting on you.
pub const ATTENTION: &str = ph::EXCLAMATION_MARK;
/// Running; spun while a command runs.
pub const RUNNING: &str = ph::CIRCLE_NOTCH;
/// Not run yet.
pub const PENDING: &str = ph::DOTS_THREE;

/// Refresh now.
pub const REFRESH: &str = ph::ARROWS_CLOCKWISE;
/// Settings.
pub const SETTINGS: &str = ph::SLIDERS_HORIZONTAL;
/// Back.
pub const BACK: &str = ph::CARET_LEFT;
/// Forward, or open the next thing.
pub const FORWARD: &str = ph::ARROW_RIGHT;
/// Open in the browser.
pub const EXTERNAL: &str = ph::ARROW_SQUARE_OUT;
/// Run a command.
pub const RUN: &str = ph::PLAY;
/// Stop a command.
pub const STOP: &str = ph::STOP;
/// Add.
pub const ADD: &str = ph::PLUS;
/// Remove.
pub const REMOVE: &str = ph::X;
/// Locked: managed somewhere else.
pub const LOCKED: &str = ph::LOCK_SIMPLE;
/// Copy to the clipboard.
pub const COPY: &str = ph::COPY;
/// Search and quick actions.
pub const SEARCH: &str = ph::MAGNIFYING_GLASS;

/// A tracked repository.
pub const REPOSITORY: &str = ph::BOOK_BOOKMARK;
/// A branch.
pub const BRANCH: &str = ph::GIT_BRANCH;
/// A commit.
pub const COMMIT: &str = ph::GIT_COMMIT;
/// A pull request.
pub const PULL_REQUEST: &str = ph::GIT_PULL_REQUEST;
/// Command output.
pub const TERMINAL: &str = ph::TERMINAL_WINDOW;
/// History.
pub const HISTORY: &str = ph::CLOCK_COUNTER_CLOCKWISE;
/// Diagnostics.
pub const DIAGNOSTICS: &str = ph::PULSE;
/// General settings.
pub const GENERAL: &str = ph::GEAR_SIX;
/// Notifications.
pub const NOTIFY: &str = ph::BELL;

/// The `tests` signoff category.
pub const TESTS: &str = ph::FLASK;
/// The `lint` signoff category.
pub const LINT: &str = ph::TEXT_AA;
/// The `security` signoff category.
pub const SECURITY: &str = ph::SHIELD_CHECK;
/// Any other signoff category.
pub const CHECK_OTHER: &str = ph::LIST_CHECKS;

/// The icon for a signoff category, as `gh signoff` names them.
#[must_use]
pub fn for_category(category: &str) -> &'static str {
    match category {
        "tests" | "test" => TESTS,
        "lint" | "format" | "fmt" => LINT,
        "security" | "audit" => SECURITY,
        _ => CHECK_OTHER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_have_their_own_icons() {
        assert_eq!(for_category("tests"), TESTS);
        assert_eq!(for_category("security"), SECURITY);
        assert_eq!(for_category("docs"), CHECK_OTHER);
    }

    #[test]
    fn states_have_distinct_glyphs() {
        let glyphs = [CHECK, FAIL, ATTENTION, RUNNING, PENDING];
        for (i, a) in glyphs.iter().enumerate() {
            for b in &glyphs[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
