//! Signoff's logic, with no UI.
//!
//! Everything here is plain, blocking Rust: the app calls it from a
//! background thread so the window never waits on `gh`.
//!
//! - [`gh`] talks to GitHub through the `gh` CLI, so the user's own auth
//!   and the extension's safety checks still apply. Every write goes
//!   through it.
//! - [`api`] reads repositories over GitHub's REST API in-process with the
//!   token `gh auth token` gives, using `ETag`s so an unchanged resource
//!   answers a free 304; without a token, reads go through `gh`.
//! - [`checks`] turns branch protection plus commit statuses into the rows
//!   the popover shows, and decides readiness.
//! - [`config`] and [`history`] are the two files Signoff keeps on disk.
//! - [`product`] reads each tracked repository, and [`ProductService`]
//!   patches what it reads into the shared state; `store` keeps those
//!   patches consistent with the app's own changes.
//! - [`watch_paths`] names the files under `.git` that say what is checked
//!   out, for the app to watch.
//! - [`verify`] runs a check's local command on the pull request's head
//!   commit and writes the signoff only when it passed.
//! - [`rules`] edits the signoff checks branch protection requires,
//!   leaving every other rule alone.
//! - [`discover`] finds the GitHub checkouts already on this computer.
//! - [`suggest`] proposes local commands for a check from the checkout's
//!   `Makefile`, `package.json` and the like.
//! - [`files`] reads a repository's files from disk or from a commit.
//! - [`notify`] decides which changes between two states earn a desktop
//!   notification.
//! - [`worktree`] keeps the worktrees Signoff verifies pull requests in
//!   when the person's own checkout is elsewhere, and cleans them up.
//! - [`demo`] is fixed sample data that never touches GitHub.

pub mod api;
pub mod checks;
pub mod config;
pub mod demo;
pub mod discover;
pub mod files;
pub mod gh;
mod git;
pub mod history;
pub mod notify;
pub mod product;
pub mod rules;
pub mod runner;
mod store;
pub mod suggest;
pub mod verify;
pub mod worktree;

pub use checks::{Check, CheckState, Rules, Tally};
pub use config::{AppConfig, CommandMapping, ConfigError, ConfigStore, RepositoryUpdate, TrackedRepository};
pub use gh::{CommitStatus, Environment, Gh, GhError, PrInfo, ProtectionStatus, RepoInfo};
pub use git::{WatchPaths, watch_paths};
pub use history::{ActivityEvent, ActivityKind, HistoryStore};
pub use notify::{Change, Transition, transitions};
pub use product::{Health, ProductService, ProductState, ProductSummary, PullRequestState, RepositoryState};
pub use rules::{RulesError, Write as RulesWrite};
pub use runner::{CANCELLED, Output, Runner, SystemRunner, TIMED_OUT};
pub use suggest::CommandSuggestion;
pub use verify::{Outcome, Phase, Verifier, VerifyError, VerifyEvent, VerifyRequest};
pub use worktree::Worktrees;
