//! Editing the signoff checks a branch requires.
//!
//! Branch protection holds much more than signoff: reviews, push
//! restrictions, other required checks. An edit here changes only the
//! `signoff` and `signoff/*` contexts and leaves everything else as it was.
//! When the branch has no protection yet, `gh signoff install` creates it,
//! exactly as it would from the command line.
//!
//! [`plan`] decides what to write and is pure, so it is tested without
//! GitHub; [`Gh::set_signoff_checks`] reads the branch, plans and writes.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::json;

use crate::api::path_segment;
use crate::checks::CheckState;
use crate::checks::{merge_signoff_contexts, valid_context_name};
use crate::gh::{Gh, GhError, ProtectionStatus};
use crate::history::{ActivityEvent, ActivityKind};
use crate::product::ProductService;

/// Why the required checks could not be changed.
#[derive(Debug, thiserror::Error)]
pub enum RulesError {
    /// A check name that is empty, too long, starts with `-` or has
    /// characters other than letters, digits, `-`, `_` and `.`.
    #[error("{0:?} is not a valid check name: use letters, digits, '-', '_' and '.'")]
    InvalidCheck(String),
    /// The same check twice.
    #[error("{0:?} is listed twice")]
    DuplicateCheck(String),
    /// Not a branch name git accepts.
    #[error("{0:?} is not a valid branch name")]
    InvalidBranch(String),
    /// The branch's protection cannot be read with the current sign-in, so
    /// writing it could drop rules we cannot see.
    #[error("branch protection on {0} is not readable with your GitHub permissions")]
    Unreadable(String),
    /// `gh` failed.
    #[error(transparent)]
    Gh(#[from] GhError),
}

/// What to write to GitHub for an edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    /// Nothing changes.
    Unchanged,
    /// The branch has no protection: run `gh signoff install` with these
    /// check names (no `signoff/` prefix; the bare check is implied).
    Install(Vec<String>),
    /// Protection exists without required checks: add them.
    AddRequiredChecks(Vec<String>),
    /// Replace the required contexts with these.
    ReplaceContexts(Vec<String>),
    /// No context is left: remove the required-checks rule.
    RemoveRequiredChecks,
}

/// The signoff contexts for check `names`, as given by the user: `signoff`
/// stays the bare context and any other name becomes `signoff/<name>`.
/// Sorted.
///
/// # Errors
///
/// When a name is not valid or is listed twice.
pub fn desired_contexts(names: &[String]) -> Result<Vec<String>, RulesError> {
    let mut seen = BTreeSet::new();
    for name in names {
        let name = name.trim();
        if !valid_context_name(name) {
            return Err(RulesError::InvalidCheck(name.to_owned()));
        }
        if !seen.insert(name.to_owned()) {
            return Err(RulesError::DuplicateCheck(name.to_owned()));
        }
    }
    let mut contexts: Vec<String> =
        seen.into_iter().map(|name| if name == "signoff" { name } else { format!("signoff/{name}") }).collect();
    contexts.sort();
    Ok(contexts)
}

/// What to write so that `branch` requires exactly the `desired` signoff
/// contexts, given the contexts it requires now (`existing`) and how reading
/// them went.
///
/// # Errors
///
/// When the protection is unreadable: writing it blind could drop rules.
pub fn plan(existing: &[String], protection: ProtectionStatus, desired: &[String]) -> Result<Write, RulesError> {
    match protection {
        ProtectionStatus::Unreadable => Err(RulesError::Unreadable(String::new())),
        ProtectionStatus::Absent if desired.is_empty() => Ok(Write::Unchanged),
        ProtectionStatus::Absent => Ok(Write::Install(
            desired
                .iter()
                .filter(|c| c.as_str() != "signoff")
                .map(|c| c.trim_start_matches("signoff/").to_owned())
                .collect(),
        )),
        ProtectionStatus::Ok => {
            let combined = merge_signoff_contexts(existing, desired);
            let before: BTreeSet<&String> = existing.iter().collect();
            let after: BTreeSet<&String> = combined.iter().collect();
            Ok(if before == after {
                Write::Unchanged
            } else if combined.is_empty() {
                Write::RemoveRequiredChecks
            } else if existing.is_empty() {
                Write::AddRequiredChecks(combined)
            } else {
                Write::ReplaceContexts(combined)
            })
        }
    }
}

impl Gh {
    /// Makes `branch` of `repo` require exactly the signoff checks `names`
    /// (`tests`, `lint`, or `signoff` for the bare check), keeping every
    /// other rule. Returns what was written.
    ///
    /// `dir` is the local checkout, used to validate the branch name and as
    /// the directory `gh` runs in.
    ///
    /// # Errors
    ///
    /// When a name or the branch is invalid, the protection is unreadable,
    /// or `gh` fails.
    pub fn set_signoff_checks(
        &self,
        dir: &Path,
        repo: &str,
        branch: &str,
        names: &[String],
    ) -> Result<Write, RulesError> {
        let branch = branch.trim();
        if branch.is_empty()
            || branch.starts_with('-')
            || self.run(Some(dir), None, "git", &["check-ref-format", "--branch", branch]).is_err()
        {
            return Err(RulesError::InvalidBranch(branch.to_owned()));
        }
        let desired = desired_contexts(names)?;
        let (existing, protection) = self.protection_contexts(dir, repo, &path_segment(branch));
        let write = plan(&existing, protection, &desired).map_err(|err| match err {
            RulesError::Unreadable(_) => RulesError::Unreadable(branch.to_owned()),
            other => other,
        })?;

        let base = format!("repos/{repo}/branches/{}/protection/required_status_checks", path_segment(branch));
        match &write {
            Write::Unchanged => {}
            Write::Install(checks) => {
                let mut args = vec!["signoff", "install"];
                args.extend(checks.iter().map(String::as_str));
                args.extend(["--branch", branch]);
                self.run(Some(dir), None, "gh", &args)?;
            }
            Write::AddRequiredChecks(contexts) => {
                let body = json!({ "strict": false, "contexts": contexts }).to_string();
                self.run(Some(dir), Some(&body), "gh", &["api", "--method", "PATCH", &base, "--input", "-"])?;
            }
            Write::ReplaceContexts(contexts) => {
                let body = json!({ "contexts": contexts }).to_string();
                let path = format!("{base}/contexts");
                self.run(Some(dir), Some(&body), "gh", &["api", "--method", "PUT", &path, "--input", "-"])?;
            }
            Write::RemoveRequiredChecks => {
                self.run(Some(dir), None, "gh", &["api", "--method", "DELETE", &base])?;
            }
        }
        if write != Write::Unchanged
            && let Some(api) = self.api()
        {
            // The next read must show the new rules, not the kept ones.
            api.forget_protection(repo, branch);
        }
        Ok(write)
    }
}

/// The signoff checks a branch requires now, as check names (`tests`, or
/// `signoff` for the bare check), and whether protection could be read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchChecks {
    /// Signoff check names, sorted.
    pub checks: Vec<String>,
    /// Every context the branch requires, signoff or not.
    pub contexts: Vec<String>,
    /// Whether the branch is protected at all.
    pub protected: bool,
}

/// `signoff/tests` → `tests`; the bare `signoff` stays.
fn check_name(context: &str) -> String {
    context.strip_prefix("signoff/").unwrap_or(context).to_owned()
}

impl ProductService {
    /// The signoff checks `branch` of the tracked repository `repo_id`
    /// requires. Blocks on `gh`.
    ///
    /// # Errors
    ///
    /// In demo mode, for an unknown repository, or when protection is
    /// unreadable.
    pub fn signoff_checks(&self, repo_id: &str, branch: &str) -> Result<BranchChecks, String> {
        let (gh, config, _) = self.github().ok_or("branch rules are read-only in demo mode")?;
        let repo = config.find_repository(repo_id).ok_or("that repository is no longer tracked")?;
        let (contexts, protection) = gh.protection_contexts(&repo.path, &repo.name_with_owner, &path_segment(branch));
        if protection == ProtectionStatus::Unreadable {
            return Err(RulesError::Unreadable(branch.to_owned()).to_string());
        }
        let mut checks: Vec<String> =
            contexts.iter().filter(|c| crate::checks::is_signoff_context(c)).map(|c| check_name(c)).collect();
        checks.sort();
        Ok(BranchChecks { checks, contexts, protected: protection == ProtectionStatus::Ok })
    }

    /// Makes `branch` of the tracked repository `repo_id` require exactly
    /// the signoff checks `names`, keeping every other rule, and records the
    /// change in the history. Blocks on `gh`.
    ///
    /// # Errors
    ///
    /// In demo mode, for an unknown repository, or when
    /// [`Gh::set_signoff_checks`] fails.
    pub fn set_signoff_checks(&self, repo_id: &str, branch: &str, names: &[String]) -> Result<Write, String> {
        let (gh, config, history) = self.github().ok_or("branch rules are read-only in demo mode")?;
        let repo = config.find_repository(repo_id).ok_or("that repository is no longer tracked")?;
        let write =
            gh.set_signoff_checks(&repo.path, &repo.name_with_owner, branch, names).map_err(|err| err.to_string())?;
        if write != Write::Unchanged {
            let event = ActivityEvent {
                kind: ActivityKind::Rules,
                repo_id: repo.id.clone(),
                repo: repo.name_with_owner.clone(),
                check: names.join(", "),
                state: CheckState::Success,
                detail: format!("Updated required signoff checks on {}", branch.trim()),
                ..ActivityEvent::default()
            };
            if let Err(err) = history.append(event) {
                log::warn!("could not record the rules change: {err}");
            }
        }
        Ok(write)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{Output, Runner};
    use std::io;
    use std::sync::{Arc, Mutex};

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn names_become_signoff_contexts() {
        let desired = desired_contexts(&strings(&["tests", " lint ", "signoff"])).expect("valid");
        assert_eq!(desired, strings(&["signoff", "signoff/lint", "signoff/tests"]));
    }

    #[test]
    fn bad_and_duplicate_names_are_refused() {
        assert!(matches!(desired_contexts(&strings(&["-f"])), Err(RulesError::InvalidCheck(_))));
        assert!(matches!(desired_contexts(&strings(&["a b"])), Err(RulesError::InvalidCheck(_))));
        assert!(matches!(desired_contexts(&strings(&["lint", "lint"])), Err(RulesError::DuplicateCheck(_))));
    }

    #[test]
    fn other_checks_survive_an_edit() {
        let existing = strings(&["ci/build", "signoff/tests", "signoff/old"]);
        let write = plan(&existing, ProtectionStatus::Ok, &strings(&["signoff/tests", "signoff/lint"])).expect("plan");
        assert_eq!(write, Write::ReplaceContexts(strings(&["ci/build", "signoff/lint", "signoff/tests"])));
    }

    #[test]
    fn the_same_checks_write_nothing() {
        let existing = strings(&["signoff/tests", "ci/build"]);
        let write = plan(&existing, ProtectionStatus::Ok, &strings(&["signoff/tests"])).expect("plan");
        assert_eq!(write, Write::Unchanged);
    }

    #[test]
    fn removing_the_last_check_removes_the_rule() {
        let write = plan(&strings(&["signoff/tests"]), ProtectionStatus::Ok, &[]).expect("plan");
        assert_eq!(write, Write::RemoveRequiredChecks);
    }

    #[test]
    fn removing_signoff_keeps_other_checks() {
        let write = plan(&strings(&["signoff/tests", "ci/build"]), ProtectionStatus::Ok, &[]).expect("plan");
        assert_eq!(write, Write::ReplaceContexts(strings(&["ci/build"])));
    }

    #[test]
    fn protection_without_checks_gains_them() {
        let write = plan(&[], ProtectionStatus::Ok, &strings(&["signoff/tests"])).expect("plan");
        assert_eq!(write, Write::AddRequiredChecks(strings(&["signoff/tests"])));
    }

    #[test]
    fn an_unprotected_branch_is_installed() {
        let write = plan(&[], ProtectionStatus::Absent, &strings(&["signoff", "signoff/lint"])).expect("plan");
        assert_eq!(write, Write::Install(strings(&["lint"])));
        assert_eq!(plan(&[], ProtectionStatus::Absent, &[]).expect("plan"), Write::Unchanged);
    }

    #[test]
    fn unreadable_protection_is_never_written() {
        assert!(matches!(plan(&[], ProtectionStatus::Unreadable, &[]), Err(RulesError::Unreadable(_))));
    }

    #[test]
    fn branch_names_stay_one_path_segment() {
        assert_eq!(path_segment("release/1.0"), "release%2F1.0");
        assert_eq!(path_segment("main"), "main");
    }

    /// Answers by the first matching needle and records each call with its
    /// stdin.
    struct Script {
        replies: Vec<(&'static str, &'static str, i32)>,
        calls: Mutex<Vec<(String, Option<String>)>>,
    }

    impl Runner for Script {
        fn run(&self, _dir: Option<&Path>, input: Option<&str>, program: &str, args: &[&str]) -> io::Result<Output> {
            let line = format!("{program} {}", args.join(" "));
            if let Ok(mut calls) = self.calls.lock() {
                calls.push((line.clone(), input.map(str::to_owned)));
            }
            let (_, stdout, code) =
                self.replies.iter().find(|(needle, ..)| line.contains(needle)).copied().unwrap_or(("", "", 0));
            Ok(Output { stdout: stdout.to_owned(), stderr: String::new(), code })
        }
    }

    fn script(replies: Vec<(&'static str, &'static str, i32)>) -> Arc<Script> {
        Arc::new(Script { replies, calls: Mutex::new(Vec::new()) })
    }

    #[test]
    fn writes_only_signoff_contexts_through_gh() {
        let runner = script(vec![("protection --jq", r#"["ci/build","signoff/old"]"#, 0)]);
        let gh = Gh::new(runner.clone());
        let write = gh.set_signoff_checks(Path::new("/repo"), "o/r", "release/1.0", &strings(&["tests"])).expect("set");
        assert_eq!(write, Write::ReplaceContexts(strings(&["ci/build", "signoff/tests"])));

        let calls = runner.calls.lock().expect("calls").clone();
        let (line, input) = calls.last().expect("a write");
        assert_eq!(
            line,
            "gh api --method PUT repos/o/r/branches/release%2F1.0/protection/required_status_checks/contexts --input -"
        );
        assert_eq!(input.as_deref(), Some(r#"{"contexts":["ci/build","signoff/tests"]}"#));
    }

    #[test]
    fn the_service_records_a_change_in_the_history() {
        use crate::config::{AppConfig, ConfigStore, TrackedRepository};
        use crate::history::HistoryStore;

        let dir = tempfile::tempdir().expect("tempdir");
        let repo = TrackedRepository {
            id: "r1".into(),
            path: dir.path().to_owned(),
            name_with_owner: "o/r".into(),
            default_branch: "main".into(),
            enabled: true,
            ..TrackedRepository::default()
        };
        let config = ConfigStore::with_config(
            dir.path().join("config.json"),
            AppConfig { repositories: vec![repo], ..AppConfig::default() },
        );
        let history = Arc::new(HistoryStore::open(dir.path().join("history.json")).expect("history"));
        let runner = script(vec![("protection --jq", r#"["signoff/tests"]"#, 0)]);
        let service = ProductService::new(Gh::new(runner), Arc::new(config), history.clone(), None);

        let read = service.signoff_checks("r1", "main").expect("read");
        assert_eq!(read.checks, strings(&["tests"]));
        assert!(read.protected);

        let write = service.set_signoff_checks("r1", "main", &strings(&["tests", "lint"])).expect("set");
        assert_eq!(write, Write::ReplaceContexts(strings(&["signoff/lint", "signoff/tests"])));
        let events = history.snapshot(0);
        assert_eq!(events[0].kind, ActivityKind::Rules);
        assert_eq!(events[0].detail, "Updated required signoff checks on main");

        assert!(ProductService::demo().set_signoff_checks("r1", "main", &[]).is_err());
    }

    #[test]
    fn an_invalid_branch_reaches_no_api() {
        let runner = script(vec![("check-ref-format", "", 1)]);
        let gh = Gh::new(runner.clone());
        let err = gh.set_signoff_checks(Path::new("/repo"), "o/r", "bad..name", &[]).expect_err("refused");
        assert!(matches!(err, RulesError::InvalidBranch(_)));
        assert!(runner.calls.lock().expect("calls").iter().all(|(line, _)| !line.starts_with("gh")));
    }
}
