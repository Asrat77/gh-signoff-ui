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
//! - [`product`] reads each tracked repository, and [`ProductService`]
//!   patches what it reads into the shared state; `store` keeps those
//!   patches consistent with the app's own changes.
//! - [`verify`] runs a check's local command on the pull request's head
//!   commit and writes the signoff only when it passed.
//! - [`rules`] edits the signoff checks branch protection requires,
//!   leaving every other rule alone.
//! - [`discover`] finds the GitHub checkouts already on this computer.
//! - [`suggest`] proposes local commands for a check from the checkout's
//!   `Makefile`, `package.json` and the like.
//! - [`demo`] is fixed sample data that never touches GitHub.

pub mod checks;
pub mod config;
pub mod demo;
pub mod discover;
pub mod gh;
mod git;
pub mod history;
pub mod product;
pub mod rules;
pub mod runner;
mod store;
pub mod suggest;
pub mod verify;

pub use checks::{Check, CheckState, Rules, Tally};
pub use config::{AppConfig, CommandMapping, ConfigError, ConfigStore, RepositoryUpdate, TrackedRepository};
pub use gh::{CommitStatus, Environment, Gh, GhError, PrInfo, ProtectionStatus, RepoInfo};
pub use history::{ActivityEvent, ActivityKind, HistoryStore};
pub use product::{Health, ProductService, ProductState, ProductSummary, PullRequestState, RepositoryState};
pub use rules::{RulesError, Write as RulesWrite};
pub use runner::{CANCELLED, Output, Runner, SystemRunner, TIMED_OUT};
pub use suggest::CommandSuggestion;
pub use verify::{Outcome, Phase, Verifier, VerifyError, VerifyEvent, VerifyRequest};
