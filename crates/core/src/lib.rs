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
//! - [`verify`] runs a check's local command on the pull request's head
//!   commit and writes the signoff only when it passed.
//! - [`rules`] edits the signoff checks branch protection requires,
//!   leaving every other rule alone.
//! - [`demo`] is fixed sample data that never touches GitHub.

pub mod checks;
pub mod config;
pub mod demo;
pub mod gh;
pub mod history;
pub mod product;
pub mod rules;
pub mod runner;
pub mod verify;

pub use checks::{Check, CheckState, Rules, Tally};
pub use config::{
    AppConfig, CommandMapping, ConfigError, ConfigStore, PullRequestScope, RepositoryUpdate, TrackedRepository,
};
pub use gh::{CommitStatus, Environment, Gh, GhError, PrInfo, ProtectionStatus, RepoInfo};
pub use history::{ActivityEvent, ActivityKind, HistoryStore};
pub use product::{Health, ProductService, ProductState, ProductSummary, PullRequestState, RepositoryState};
pub use rules::{RulesError, Write as RulesWrite};
pub use runner::{CANCELLED, Output, Runner, SystemRunner, TIMED_OUT};
pub use verify::{Outcome, Phase, Verifier, VerifyError, VerifyEvent, VerifyRequest};
