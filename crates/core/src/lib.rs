//! Signoff's logic, with no UI.
//!
//! Everything here is plain, blocking Rust: the app calls it from a
//! background thread so the window never waits on `gh`.
//!
//! - [`gh`] reads GitHub through the `gh` CLI (never the network directly),
//!   so the user's own auth and the extension's safety checks still apply.
//! - [`checks`] turns branch protection plus commit statuses into the rows
//!   the popover shows, and decides readiness.
//! - [`config`] and [`history`] are the two files Signoff keeps on disk.
//! - [`product`] assembles the whole multi-repository state, and
//!   [`ProductService`] keeps the latest copy of it.
//! - [`demo`] is fixed sample data that never touches GitHub.

pub mod checks;
pub mod gh;
pub mod runner;
