//! Read failures in human words.
//!
//! The core keeps the raw text of a failed read (a `gh` command line, an
//! exit code, stderr). That is useful in a bug report and useless as a
//! headline, so the UI shows [`describe`]'s sentence and keeps the raw
//! text behind a "Details" disclosure.

/// One failed read, explained.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Problem {
    /// A short sentence a person can act on.
    pub(crate) title: String,
    /// The raw text, for the disclosure.
    pub(crate) details: String,
}

/// Explains `raw`, the error of a read of `repo` (a name, `owner/name`
/// or empty for GitHub in general).
pub(crate) fn describe(repo: &str, raw: &str) -> Problem {
    let short = repo.rsplit('/').next().unwrap_or(repo);
    let of = if short.is_empty() { String::new() } else { format!(" for {short}") };
    let lower = raw.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));
    let title = if has(&["rate limit"]) {
        // Already a sentence, and says when reads resume.
        raw.to_owned()
    } else if has(&["gh auth login", "not logged", "bad credentials", "http 401", "authentication"]) {
        "GitHub needs you to sign in again. Run `gh auth login`.".to_owned()
    } else if has(&["timed out", "exit 124", "timeout"]) {
        format!("GitHub didn't answer in time{of}. Signoff will try again.")
    } else if has(&["could not resolve", "connection", "dial tcp", "network", "tls", "certificate", "unreachable"]) {
        format!("Couldn't connect to GitHub{of}. Check the network; Signoff will try again.")
    } else if has(&["launching gh", "executable file not found"]) {
        "The `gh` command isn't installed or isn't on the PATH.".to_owned()
    } else if has(&["404", "not found", "could not resolve to a repository"]) {
        match short {
            "" => "GitHub couldn't find it, or you can't see it.".to_owned(),
            name => format!("GitHub couldn't find {name}, or you can't see it."),
        }
    } else {
        format!("Couldn't read{}.", if short.is_empty() { " GitHub".to_owned() } else { format!(" {short}") })
    };
    Problem { title, details: raw.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMEOUT: &str = "gh pr list --state open --limit 31 --json number: exit 124: gh timed out after 30s";

    #[test]
    fn a_timeout_names_the_repository_and_hides_the_command() {
        let problem = describe("acme/bella_sports", TIMEOUT);
        assert_eq!(problem.title, "GitHub didn't answer in time for bella_sports. Signoff will try again.");
        assert_eq!(problem.details, TIMEOUT);
        assert!(!problem.title.contains("exit 124"));
    }

    #[test]
    fn a_signed_out_gh_says_how_to_sign_in() {
        assert!(describe("a/b", "gh: not logged in").title.contains("gh auth login"));
    }

    #[test]
    fn a_rate_limit_message_is_kept_as_it_is() {
        let raw = "GitHub's API rate limit is used up; Signoff reads again at 10:00.";
        assert_eq!(describe("a/b", raw).title, raw);
    }

    #[test]
    fn an_unknown_failure_still_reads_as_a_sentence() {
        assert_eq!(describe("a/b", "boom").title, "Couldn't read b.");
        assert_eq!(describe("", "boom").title, "Couldn't read GitHub.");
    }
}
