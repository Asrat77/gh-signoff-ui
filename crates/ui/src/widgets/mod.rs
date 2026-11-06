//! Signoff's widgets.
//!
//! Each widget reads its colours from [`crate::palette`], so it follows the
//! light or dark appearance by itself. Rows and buttons are drawn with the
//! painter rather than egui's stock frames, which keeps their shapes exact.

mod button;
mod check_row;
mod chip;
mod empty_state;
mod eyes;
mod header;
mod log_view;
mod paint;
mod pr_row;
mod progress;
mod switch;
mod toast;
mod toggle_row;

pub use button::{AddButton, Button, ButtonKind, IconButton, IconButtonKind};
pub use check_row::{CheckRow, CheckRowResponse};
pub use chip::Chip;
pub use empty_state::EmptyState;
pub use eyes::{Eyes, Mood};
pub use header::{Band, Headline, sheet};
pub use log_view::{LogLine, LogView};
pub use paint::{icon_tile, spinner};
pub use pr_row::PrRow;
pub use progress::{Segment, Segments};
pub use switch::Switch;
pub use toast::{Toast, show_toasts, toast};
pub use toggle_row::ToggleRow;
