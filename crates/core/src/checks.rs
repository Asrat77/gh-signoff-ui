//! Signoff contexts, resolved against branch protection.
//!
//! Which checks are *required* is the one place Signoff deliberately departs
//! from `gh signoff status`. That command always seeds its list with the bare
//! `signoff` context, so on a repository whose protection requires only
//! `signoff/tests` it prints a permanent ✗ for `signoff`. Signoff answers
//! "can I merge", and merge is gated by branch protection alone, so:
//!
//! - protection readable: required = the signoff contexts protection lists
//! - protection unreadable (no admin): fall back to the CLI's assumption and
//!   flag readiness as unverified rather than invent a number
//!
//! Contexts present on the commit but not required are still shown, marked
//! not required, because seeing a stray signoff is useful.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::gh::CommitStatus;

/// The state of one status context.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", from = "String")]
pub enum CheckState {
    /// Signed off.
    Success,
    /// Signed off as failed.
    Failure,
    /// Errored.
    Error,
    /// Started, or expected and not yet reported.
    Pending,
    /// Nothing posted for this context.
    #[default]
    Absent,
}

impl CheckState {
    /// Parses either API's spelling: REST's `success`, GraphQL's `SUCCESS`
    /// and `EXPECTED`. Anything unknown reads as pending: not signed off.
    pub fn parse(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "success" => Self::Success,
            "failure" => Self::Failure,
            "error" => Self::Error,
            "absent" | "" => Self::Absent,
            _ => Self::Pending,
        }
    }

    /// Failure or error: something to fix, not just something to do.
    pub fn is_failed(self) -> bool {
        matches!(self, Self::Failure | Self::Error)
    }
}

impl From<String> for CheckState {
    fn from(value: String) -> Self {
        Self::parse(&value)
    }
}

/// One signoff context resolved against a commit.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Check {
    /// `signoff` or `signoff/tests`.
    pub context: String,
    /// The display name: `signoff` or `tests`.
    pub name: String,
    /// The badge colour slot, from [`category_for`].
    pub category: String,
    /// The latest state posted for it.
    pub state: CheckState,
    /// Whether branch protection requires it.
    pub required: bool,
    /// Who posted the latest state.
    pub actor: String,
    /// Their avatar.
    pub avatar_url: String,
    /// When.
    pub at: Option<Timestamp>,
    /// The status description.
    pub description: String,
    /// The exact CLI command that signs this off, shown verbatim.
    pub command: String,
    /// The local command mapped to it in the settings.
    pub local_command: String,
    /// Whether a local command is mapped.
    pub configured: bool,
}

/// What branch protection requires.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Rules {
    /// The protected branch.
    pub branch: String,
    /// Every required context, signoff or not.
    pub contexts: Vec<String>,
    /// Just the signoff ones.
    pub signoff_contexts: Vec<String>,
    /// False only when the API refused (needs admin).
    pub readable: bool,
    /// The branch has protection rules at all.
    pub protected: bool,
    /// At least one signoff context gates merge.
    pub enforced: bool,
    /// The `gh signoff install` command that would produce these rules.
    pub install_command: String,
}

/// How many required checks pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// Required checks that succeeded.
    pub passed: usize,
    /// Required checks.
    pub total: usize,
    /// The first required check that has not succeeded.
    pub blocking: Option<String>,
    /// Every required check succeeded (and there is at least one).
    pub ready: bool,
}

/// Mirrors gh-signoff's own rendering: `signoff/tests` shows as `tests`, and
/// the bare context keeps the name `signoff`.
pub fn display_name(context: &str) -> &str {
    if context == "signoff" { context } else { context.strip_prefix("signoff/").unwrap_or(context) }
}

/// Whether a status context is one gh-signoff writes.
pub fn is_signoff_context(context: &str) -> bool {
    context == "signoff" || context.starts_with("signoff/")
}

/// The badge colour slot. Three known categories, and a stable slot for
/// anything a team invents so a custom name keeps one colour.
pub fn category_for(name: &str) -> &'static str {
    match name {
        "tests" | "test" => "tests",
        "lint" | "rubocop" | "standard" | "format" => "lint",
        "security" | "audit" | "brakeman" => "security",
        "signoff" => "default",
        other => {
            let sum: u32 = other.chars().map(u32::from).sum();
            ["alt1", "alt2", "alt3"][(sum % 3) as usize]
        }
    }
}

/// The command a user types to sign this context off.
pub fn signoff_command(context: &str) -> String {
    match context.strip_prefix("signoff/") {
        Some(name) => format!("gh signoff {name}"),
        None => "gh signoff".to_owned(),
    }
}

/// Who signed off. The status API does not always report a creator (it was
/// `null` for statuses written by `gh signoff` through a user OAuth token),
/// but gh-signoff writes `<user> signed off` as the description.
pub fn actor_from(status: &CommitStatus) -> String {
    if !status.creator.is_empty() {
        return status.creator.clone();
    }
    status.description.strip_suffix(" signed off").filter(|name| !name.is_empty()).unwrap_or_default().to_owned()
}

/// The latest status per signoff context. The API returns newest first, but
/// this does not rely on it.
pub fn latest_signoffs(statuses: &[CommitStatus]) -> BTreeMap<String, CommitStatus> {
    let mut latest: BTreeMap<String, CommitStatus> = BTreeMap::new();
    for status in statuses.iter().filter(|s| is_signoff_context(&s.context)) {
        match latest.get(&status.context) {
            Some(previous) if previous.created_at >= status.created_at => {}
            _ => {
                latest.insert(status.context.clone(), status.clone());
            }
        }
    }
    latest
}

/// Turns branch protection plus the statuses on a commit into rows.
///
/// When protection is not `readable`, the bare `signoff` context is assumed
/// required: the safest guess, reporting "not ready" for a commit that might
/// be mergeable rather than the reverse. Signed-off contexts that are not
/// required are included and marked so. Required rows come first, then by
/// name.
pub fn resolve_checks(
    signoff_contexts: &[String],
    latest: &BTreeMap<String, CommitStatus>,
    readable: bool,
) -> Vec<Check> {
    let required: BTreeSet<&str> =
        if readable { signoff_contexts.iter().map(String::as_str).collect() } else { BTreeSet::from(["signoff"]) };

    let mut seen = BTreeSet::new();
    let mut checks = Vec::new();
    let base: Vec<&str> =
        if readable { signoff_contexts.iter().map(String::as_str).collect() } else { vec!["signoff"] };
    // `latest` is a BTreeMap, so the extras arrive sorted.
    for context in base.into_iter().chain(latest.keys().map(String::as_str)) {
        if !seen.insert(context) {
            continue;
        }
        let name = display_name(context);
        let mut check = Check {
            context: context.to_owned(),
            name: name.to_owned(),
            category: category_for(name).to_owned(),
            required: required.contains(context),
            command: signoff_command(context),
            ..Check::default()
        };
        if let Some(status) = latest.get(context) {
            check.state = status.state;
            check.actor = actor_from(status);
            check.avatar_url = status.avatar_url.clone();
            check.description = status.description.clone();
            check.at = Some(status.created_at);
        }
        checks.push(check);
    }
    checks.sort_by(|a, b| b.required.cmp(&a.required).then_with(|| a.name.cmp(&b.name)));
    checks
}

/// Counts only required checks: an optional context that happens to be
/// green must never make a blocked pull request look ready.
pub fn tally(checks: &[Check]) -> Tally {
    let mut t = Tally::default();
    for check in checks.iter().filter(|c| c.required) {
        t.total += 1;
        if check.state == CheckState::Success {
            t.passed += 1;
        } else if t.blocking.is_none() {
            t.blocking = Some(check.name.clone());
        }
    }
    t.ready = t.total > 0 && t.passed == t.total;
    t
}

/// The `gh signoff install` invocation that would produce these rules.
pub fn build_install_command(signoff_contexts: &[String], branch: &str) -> String {
    let mut command = "gh signoff install".to_owned();
    for context in signoff_contexts.iter().filter(|c| c.as_str() != "signoff") {
        command.push(' ');
        command.push_str(context.strip_prefix("signoff/").unwrap_or(context));
    }
    if !branch.is_empty() {
        command.push_str(" --branch ");
        command.push_str(branch);
    }
    command
}

/// Rules for `branch` from the contexts protection requires.
pub fn rules_for(branch: &str, contexts: Vec<String>, protection: crate::gh::ProtectionStatus) -> Rules {
    use crate::gh::ProtectionStatus;
    let signoff_contexts: Vec<String> = contexts.iter().filter(|c| is_signoff_context(c)).cloned().collect();
    Rules {
        branch: branch.to_owned(),
        install_command: build_install_command(&signoff_contexts, branch),
        enforced: !signoff_contexts.is_empty(),
        readable: protection != ProtectionStatus::Unreadable,
        protected: protection == ProtectionStatus::Ok,
        contexts,
        signoff_contexts,
    }
}

/// Whether a context name is safe to pass to `gh` as an argument. Names
/// reach the command line, so anything that could change its meaning is
/// refused, including a leading dash, which gh would parse as an option
/// (gh-signoff's `-f` skips its check that the commit is on a remote).
pub fn valid_context_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The contexts to require after an edit: every non-signoff context already
/// required is kept, and the signoff ones become exactly `desired`. Sorted
/// and without duplicates.
pub fn merge_signoff_contexts(existing: &[String], desired: &[String]) -> Vec<String> {
    existing
        .iter()
        .filter(|c| !is_signoff_context(c))
        .chain(desired)
        .filter(|c| !c.is_empty())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: CheckState) -> CommitStatus {
        CommitStatus { state, creator: "you".into(), created_at: Timestamp::now(), ..CommitStatus::default() }
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn uses_protection_when_readable() {
        let latest = BTreeMap::from([("signoff/tests".to_owned(), status(CheckState::Success))]);
        let checks = resolve_checks(&strings(&["signoff/tests", "signoff/lint"]), &latest, true);
        assert_eq!(checks.len(), 2);
        assert!(checks.iter().all(|c| c.required));
        assert_eq!(tally(&checks), Tally { passed: 1, total: 2, blocking: Some("lint".into()), ready: false });
    }

    /// The bare `signoff` context must not be required when protection is
    /// readable and omits it.
    #[test]
    fn bare_signoff_not_required_when_protection_omits_it() {
        let latest = BTreeMap::from([("signoff/tests".to_owned(), status(CheckState::Success))]);
        let t = tally(&resolve_checks(&strings(&["signoff/tests"]), &latest, true));
        assert_eq!(t.total, 1);
        assert!(t.ready);
    }

    #[test]
    fn unreadable_protection_falls_back_to_bare_signoff() {
        let checks = resolve_checks(&[], &BTreeMap::new(), false);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].context, "signoff");
        assert!(checks[0].required);
        assert!(!tally(&checks).ready);
    }

    /// A green optional context must never make a blocked PR look mergeable.
    #[test]
    fn optional_context_shown_but_not_counted() {
        let latest = BTreeMap::from([
            ("signoff/tests".to_owned(), status(CheckState::Absent)),
            ("signoff/docs".to_owned(), status(CheckState::Success)),
        ]);
        let checks = resolve_checks(&strings(&["signoff/tests"]), &latest, true);
        assert_eq!(checks.len(), 2);
        let docs = checks.iter().find(|c| c.name == "docs").expect("docs listed");
        assert!(!docs.required);
        let t = tally(&checks);
        assert_eq!((t.passed, t.total, t.ready), (0, 1, false));
    }

    #[test]
    fn only_success_counts() {
        for state in [CheckState::Failure, CheckState::Error, CheckState::Pending, CheckState::Absent] {
            let latest = BTreeMap::from([("signoff/tests".to_owned(), status(state))]);
            assert!(!tally(&resolve_checks(&strings(&["signoff/tests"]), &latest, true)).ready, "{state:?}");
        }
    }

    #[test]
    fn display_name_and_command() {
        for (context, name, command) in [
            ("signoff", "signoff", "gh signoff"),
            ("signoff/tests", "tests", "gh signoff tests"),
            ("signoff/design-review", "design-review", "gh signoff design-review"),
        ] {
            assert_eq!(display_name(context), name);
            assert_eq!(signoff_command(context), command);
        }
    }

    #[test]
    fn install_command() {
        assert_eq!(
            build_install_command(&strings(&["signoff/tests", "signoff/lint"]), "main"),
            "gh signoff install tests lint --branch main"
        );
        // The bare context is implied and must not appear as an argument.
        assert_eq!(build_install_command(&strings(&["signoff"]), "main"), "gh signoff install --branch main");
    }

    #[test]
    fn context_names_reject_shell_metacharacters() {
        for bad in [
            "tests;rm -rf /",
            "tests rm",
            "--commit",
            "$(whoami)",
            "`id`",
            "tests|cat",
            "tests&",
            "a/b",
            "tests\n",
            "'quoted'",
            "",
        ] {
            assert!(!valid_context_name(bad), "{bad:?}");
        }
        for good in ["tests", "design-review", "lint_2", "v1.2"] {
            assert!(valid_context_name(good), "{good:?}");
        }
    }

    #[test]
    fn actor_falls_back_to_description() {
        for (creator, description, want) in [
            ("octocat", "Asrat signed off", "octocat"),
            ("", "Asrat signed off", "Asrat"),
            ("", "", ""),
            ("", "something else entirely", ""),
        ] {
            let s =
                CommitStatus { creator: creator.into(), description: description.into(), ..CommitStatus::default() };
            assert_eq!(actor_from(&s), want);
        }
    }

    #[test]
    fn merging_keeps_external_checks() {
        let got = merge_signoff_contexts(
            &strings(&["ci/build", "signoff/lint", "signoff-ci", "ci/build"]),
            &strings(&["signoff/tests"]),
        );
        assert_eq!(got, strings(&["ci/build", "signoff-ci", "signoff/tests"]));
    }

    #[test]
    fn latest_status_wins_per_context() {
        let older = CommitStatus {
            context: "signoff/tests".into(),
            state: CheckState::Failure,
            created_at: Timestamp::from_second(1).expect("ts"),
            ..CommitStatus::default()
        };
        let newer = CommitStatus {
            state: CheckState::Success,
            created_at: Timestamp::from_second(2).expect("ts"),
            ..older.clone()
        };
        let ci = CommitStatus { context: "ci/build".into(), ..older.clone() };
        let latest = latest_signoffs(&[older, newer, ci]);
        assert_eq!(latest.len(), 1);
        assert_eq!(latest["signoff/tests"].state, CheckState::Success);
    }

    #[test]
    fn states_parse_from_either_api() {
        assert_eq!(CheckState::parse("SUCCESS"), CheckState::Success);
        assert_eq!(CheckState::parse("EXPECTED"), CheckState::Pending);
        assert_eq!(serde_json::from_str::<CheckState>("\"failure\"").expect("parse"), CheckState::Failure);
        assert_eq!(serde_json::to_string(&CheckState::Absent).expect("json"), "\"absent\"");
    }
}
