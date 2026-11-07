//! A run that did not sign off, in words that say whose problem it is.
//!
//! A check that fails (a test, a lint, a vulnerability scan exiting
//! non-zero) is the check doing its job, not Signoff breaking. So the page
//! leads with what the check found and what to do next, and keeps
//! Signoff's own failures (the worktree, the pull request moving, `gh
//! signoff`) worded as those. [`agent_prompt`] turns a failed check into
//! a request a coding agent can act on.

use signoff_core::{Ending, Outcome, PullRequestState};
use signoff_ui::Status;

/// Output lines a copied prompt keeps, from the end.
const PROMPT_LINES: usize = 200;

/// How a finished run that did not sign off reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    /// The headline, such as "security failed".
    pub(crate) title: String,
    /// What happened and what it means, in a sentence or two.
    pub(crate) detail: String,
    /// The colour and glyph: failing for the check, attention for
    /// Signoff, neutral for a stop.
    pub(crate) status: Status,
    /// The person's command failed, so the fix is in their code and an
    /// agent can help.
    pub(crate) check_failed: bool,
    /// It ran past its timeout, which can be raised.
    pub(crate) timed_out: bool,
}

/// How `result`, the end of a run of `check`, reads; `None` when it
/// signed off.
pub(crate) fn describe(check: &str, result: &Result<Outcome, String>) -> Option<Failure> {
    let outcome = match result {
        Ok(outcome) if outcome.signed_off() => return None,
        Ok(outcome) => outcome,
        Err(refused) => {
            return Some(Failure {
                title: format!("Couldn't run {check}"),
                detail: sentence(refused),
                status: Status::Attention,
                check_failed: false,
                timed_out: false,
            });
        }
    };
    let command = if outcome.command.is_empty() { "The command" } else { outcome.command.as_str() };
    let why = outcome.message.as_deref().map(sentence).unwrap_or_default();
    let failure = match outcome.ending {
        Ending::CheckFailed => Failure {
            title: format!("{check} failed"),
            detail: format!(
                "{command} exited with {}. That's the check's own result, not a Signoff error, and nothing was \
                 signed off.",
                outcome.code
            ),
            status: Status::Failing,
            check_failed: true,
            timed_out: false,
        },
        Ending::TimedOut => Failure {
            title: format!("{check} timed out"),
            detail: format!("{why} Nothing was signed off. Raise the limit in Commands if it needs longer."),
            status: Status::Failing,
            check_failed: true,
            timed_out: true,
        },
        Ending::Stopped => Failure {
            title: format!("Stopped {check}"),
            detail: "You stopped it, so nothing was signed off.".to_owned(),
            status: Status::Neutral,
            check_failed: false,
            timed_out: false,
        },
        Ending::Interrupted | Ending::SignedOff => {
            let passed = !outcome.command.is_empty();
            Failure {
                title: if passed {
                    format!("{check} passed, but wasn't signed off")
                } else {
                    format!("Couldn't run {check}")
                },
                detail: why,
                status: Status::Attention,
                check_failed: false,
                timed_out: false,
            }
        }
    };
    Some(failure)
}

/// `text` with a capital and a full stop.
fn sentence(text: &str) -> String {
    let text = text.trim();
    let mut chars = text.chars();
    let mut out = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
        None => return String::new(),
    };
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

/// What the command printed, without colour codes: the lines after the
/// last `$ command` echo, so Signoff's own steps before it (preparing a
/// worktree) are left out.
pub(crate) fn command_output(command: &str, lines: &[String]) -> Vec<String> {
    let echo = format!("$ {command}");
    let start = lines.iter().rposition(|line| !command.is_empty() && *line == echo).map_or(0, |at| at + 1);
    lines[start..].iter().map(|line| signoff_ui::ansi::strip(line)).collect()
}

/// The run's output as plain text, to paste somewhere, with the command
/// and how it ended.
pub(crate) fn plain_output(outcome: Option<&Outcome>, lines: &[String]) -> String {
    let mut text: String = lines.iter().map(|line| signoff_ui::ansi::strip(line) + "\n").collect();
    if let Some(outcome) = outcome.filter(|outcome| !outcome.signed_off()) {
        text.push_str(&format!("[exit {}]\n", outcome.code));
    }
    text
}

/// A prompt that asks a coding agent to fix the failed check: where it
/// ran, the command, how it ended, and the end of its output.
pub(crate) fn agent_prompt(pr: &PullRequestState, check: &str, outcome: &Outcome, lines: &[String]) -> String {
    let output = command_output(&outcome.command, lines);
    let skipped = output.len().saturating_sub(PROMPT_LINES);
    let ended = if outcome.ending == Ending::TimedOut {
        outcome.message.as_deref().map_or_else(|| "timed out".to_owned(), sentence)
    } else {
        format!("exited with {}", outcome.code)
    };
    let mut prompt = format!(
        "The `{check}` check failed locally on a pull request. Please find the cause and fix it.\n\n\
         - Pull request: {repo}#{number} \"{title}\"\n\
         - Branch: `{branch}` at {sha}\n\
         - Command: `{command}` (run from the repository root)\n\
         - Result: {ended}\n",
        repo = pr.repo,
        number = pr.pr.number,
        title = pr.pr.title,
        branch = pr.pr.head_ref,
        sha = crate::glance::short_sha(&pr.pr.head_sha),
        command = outcome.command,
    );
    if !pr.repo_path.as_os_str().is_empty() {
        prompt.push_str(&format!("- Local checkout: {}\n", pr.repo_path.display()));
    }
    prompt.push_str("\nOutput");
    if skipped > 0 {
        prompt.push_str(&format!(" (last {PROMPT_LINES} lines)"));
    }
    prompt.push_str(":\n\n```text\n");
    for line in &output[skipped..] {
        prompt.push_str(line);
        prompt.push('\n');
    }
    prompt.push_str("```\n\n");
    prompt.push_str(&format!(
        "Fix the code or dependencies so `{}` exits 0. Don't skip, silence or loosen the check to make it pass. \
         Run the command again to confirm, then summarise what you changed.\n",
        outcome.command
    ));
    prompt
}

#[cfg(test)]
mod tests {
    use signoff_core::Phase;

    use super::*;

    fn failed(code: i32) -> Outcome {
        Outcome {
            phase: Phase::Run,
            code,
            message: Some(format!("the command exited with {code}, so nothing was signed off")),
            ending: Ending::CheckFailed,
            command: "bundle exec bundle-audit".into(),
            entry: Some("e1".into()),
        }
    }

    fn pr() -> PullRequestState {
        let mut pr =
            PullRequestState { repo: "acme/widgets".into(), repo_path: "/src/widgets".into(), ..Default::default() };
        pr.pr.number = 22;
        pr.pr.title = "Bump rack".into();
        pr.pr.head_ref = "bump-rack".into();
        pr.pr.head_sha = "3db401c9aa".into();
        pr
    }

    #[test]
    fn a_failed_check_reads_as_the_checks_result_not_an_app_error() {
        let failure = describe("security", &Ok(failed(1))).expect("a failure");
        assert_eq!(failure.title, "security failed");
        assert!(failure.detail.starts_with("bundle exec bundle-audit exited with 1"), "{}", failure.detail);
        assert!(failure.detail.contains("not a Signoff error, and nothing was signed off"));
        assert_eq!(failure.status, Status::Failing);
        assert!(failure.check_failed);
    }

    #[test]
    fn signoff_failing_after_a_passing_check_says_the_check_passed() {
        let outcome = Outcome {
            phase: Phase::Checkout,
            code: 1,
            message: Some("the pull request moved to aaaaaaa while the command ran; nothing was signed off".into()),
            ending: Ending::Interrupted,
            command: "make test".into(),
            entry: None,
        };
        let failure = describe("tests", &Ok(outcome)).expect("a failure");
        assert_eq!(failure.title, "tests passed, but wasn't signed off");
        assert!(failure.detail.starts_with("The pull request moved"));
        assert_eq!(failure.status, Status::Attention);
        assert!(!failure.check_failed);
    }

    #[test]
    fn a_refused_run_and_a_stop_read_as_such() {
        let refused = describe("tests", &Err("another run is using this repository's worktree".into())).expect("one");
        assert_eq!(refused.title, "Couldn't run tests");
        assert_eq!(refused.detail, "Another run is using this repository's worktree.");
        let stopped = Outcome { ending: Ending::Stopped, code: signoff_core::CANCELLED, ..Outcome::default() };
        assert_eq!(describe("tests", &Ok(stopped)).expect("one").status, Status::Neutral);
        assert!(describe("tests", &Ok(Outcome { phase: Phase::Signoff, ..Outcome::default() })).is_none());
    }

    #[test]
    fn the_command_output_leaves_out_signoffs_own_steps_and_colours() {
        let lines = [
            "preparing Signoff's worktree at 3db401c".to_owned(),
            "$ bundle exec bundle-audit".to_owned(),
            "\u{1b}[31mVulnerabilities found!\u{1b}[0m".to_owned(),
        ];
        assert_eq!(command_output("bundle exec bundle-audit", &lines), ["Vulnerabilities found!"]);
    }

    #[test]
    fn the_agent_prompt_has_everything_needed_to_fix_it() {
        let lines = [
            "$ bundle exec bundle-audit".to_owned(),
            "Name: rack".to_owned(),
            "Solution: update to '>= 0.8.2'".to_owned(),
            "Vulnerabilities found!".to_owned(),
        ];
        let prompt = agent_prompt(&pr(), "security", &failed(1), &lines);
        for needle in [
            "The `security` check failed",
            "acme/widgets#22 \"Bump rack\"",
            "`bump-rack` at 3db401c",
            "Command: `bundle exec bundle-audit`",
            "Result: exited with 1",
            "Local checkout: /src/widgets",
            "```text\nName: rack\nSolution: update to '>= 0.8.2'\nVulnerabilities found!\n```",
            "Don't skip, silence or loosen the check",
        ] {
            assert!(prompt.contains(needle), "missing {needle:?} in:\n{prompt}");
        }
        assert!(!prompt.contains("$ bundle exec"), "the echo is not output");
    }

    #[test]
    fn a_long_output_keeps_its_end_in_the_prompt() {
        let lines: Vec<String> = (0..500).map(|i| format!("line {i}")).collect();
        let prompt = agent_prompt(&pr(), "tests", &failed(2), &lines);
        assert!(prompt.contains("(last 200 lines)"));
        assert!(prompt.contains("line 499") && !prompt.contains("line 299\n"));
    }

    #[test]
    fn plain_output_ends_with_the_exit_code() {
        let text = plain_output(Some(&failed(1)), &["$ x".into(), "\u{1b}[1mboom\u{1b}[0m".into()]);
        assert_eq!(text, "$ x\nboom\n[exit 1]\n");
    }
}
