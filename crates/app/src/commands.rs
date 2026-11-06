//! What the command palette offers and how a query ranks it, worked out
//! with no UI so the ordering is tested without a window.
//!
//! Ported from the old web app's `buildPaletteActions` and `fuzzyScore`.

use std::path::PathBuf;

use signoff_core::{Check, CheckState, ProductState, PullRequestState};

use crate::glance;
use crate::settings::Section;

/// What running an action does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// Show the pull request's page.
    OpenPage { repo_id: String, number: u64 },
    /// Show the pull request's page and run and sign off one check.
    Run { repo_id: String, number: u64, check: String },
    /// Run `gh pr checkout` for the pull request.
    CheckOut { repo_id: String, number: u64, path: PathBuf },
    /// Copy a `gh signoff` command to the clipboard.
    Copy(String),
    /// Open a web page.
    OpenUrl(String),
    /// Read GitHub again, checking `gh` too.
    Refresh,
    /// Open a settings section.
    Settings(Section),
}

/// One line of the palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Action {
    /// The bold first line.
    pub(crate) label: String,
    /// The muted second line, also searched.
    pub(crate) meta: String,
    /// What it does.
    pub(crate) command: Command,
}

impl Action {
    fn new(label: impl Into<String>, meta: impl Into<String>, command: Command) -> Self {
        Self { label: label.into(), meta: meta.into(), command }
    }
}

/// Every action available now, in the order ties keep: per pull request
/// its page, what to do about its checks, then the global ones.
///
/// `at_head` says whether a pull request's checkout is on its head commit
/// (the demo pretends so after a simulated checkout).
pub(crate) fn actions(state: &ProductState, at_head: impl Fn(&PullRequestState) -> bool) -> Vec<Action> {
    let mut out = Vec::new();
    for pr in &state.pull_requests {
        let (repo_id, number) = (pr.repo_id.clone(), pr.pr.number);
        out.push(Action::new(
            format!("#{number} {}", pr.pr.title),
            pr.repo.clone(),
            Command::OpenPage { repo_id: repo_id.clone(), number },
        ));
        let at_head = at_head(pr);
        let open: Vec<&Check> = glance::required(pr).filter(|check| check.state != CheckState::Success).collect();
        let runnable = open.iter().any(|check| check.configured);
        if !at_head && runnable && !pr.repo_path.as_os_str().is_empty() {
            out.push(Action::new(
                format!("Check out #{number}"),
                format!("{} {}", pr.repo, pr.pr.head_ref),
                Command::CheckOut { repo_id: repo_id.clone(), number, path: pr.repo_path.clone() },
            ));
        }
        for check in open {
            if check.configured && at_head {
                out.push(Action::new(
                    format!("Run & sign off {} on #{number}", check.name),
                    pr.repo.clone(),
                    Command::Run { repo_id: repo_id.clone(), number, check: check.name.clone() },
                ));
            }
            out.push(Action::new(
                format!("Copy {}", check.command),
                format!("{} #{number}", pr.repo),
                Command::Copy(check.command.clone()),
            ));
        }
        out.push(Action::new(
            format!("Open #{number} on GitHub"),
            pr.repo.clone(),
            Command::OpenUrl(pr.pr.url.clone()),
        ));
    }
    out.push(Action::new("Refresh all repositories", "status", Command::Refresh));
    for (label, section) in [
        ("Manage repositories", Section::Repositories),
        ("Configure commands", Section::Commands),
        ("Edit branch rules", Section::BranchRules),
        ("Open activity history", Section::History),
        ("Open general settings", Section::General),
    ] {
        out.push(Action::new(label, "settings", Command::Settings(section)));
    }
    out
}

/// How well `query` matches `candidate` (both lowercase), or `None` when
/// it does not. A substring beats scattered letters, and earlier beats
/// later; scattered letters score more when adjacent and near the start.
pub(crate) fn fuzzy_score(candidate: &str, query: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    if let Some(at) = candidate.find(query) {
        let chars = i32::try_from(candidate[..at].chars().count()).unwrap_or(i32::MAX);
        return Some(1000 - chars);
    }
    let mut score = 0;
    let mut cursor = 0usize;
    let mut previous: Option<usize> = None;
    let letters: Vec<char> = candidate.chars().collect();
    for wanted in query.chars() {
        let found = letters.iter().skip(cursor).position(|c| *c == wanted)? + cursor;
        score += if previous.is_some_and(|p| found == p + 1) { 8 } else { 2 };
        score -= i32::try_from(found - cursor).unwrap_or(i32::MAX);
        previous = Some(found);
        cursor = found + 1;
    }
    Some(score)
}

/// The indexes of `actions` that match `query`, best first; equal scores
/// keep their order. An empty query keeps everything in order.
pub(crate) fn rank(actions: &[Action], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    let mut scored: Vec<(usize, i32)> = actions
        .iter()
        .enumerate()
        .filter_map(|(order, action)| {
            let text = format!("{} {}", action.label, action.meta).to_lowercase();
            fuzzy_score(&text, &query).map(|score| (order, score))
        })
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().map(|(order, _)| order).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use signoff_core::ProductService;

    fn demo() -> ProductState {
        ProductService::demo().refresh()
    }

    fn has(actions: &[Action], command: impl Fn(&Command) -> bool) -> bool {
        actions.iter().any(|a| command(&a.command))
    }

    #[test]
    fn a_substring_beats_scattered_letters_and_earlier_beats_later() {
        let exact = fuzzy_score("open settings", "set").expect("match");
        let early = fuzzy_score("set it", "set").expect("match");
        let scattered = fuzzy_score("s e t", "set").expect("match");
        assert!(early > exact, "earlier wins");
        assert!(exact > scattered, "a substring beats scattered");
        assert_eq!(fuzzy_score("abc", "z"), None);
        assert_eq!(fuzzy_score("abc", ""), Some(0));
        assert!(fuzzy_score("abcd", "abd") > fuzzy_score("a b c d", "abd"), "adjacent letters score more");
    }

    #[test]
    fn letters_must_appear_in_order() {
        assert_eq!(fuzzy_score("settings", "gt"), None);
        assert!(fuzzy_score("settings", "stg").is_some());
    }

    #[test]
    fn ranking_puts_the_best_first_and_keeps_ties_in_order() {
        let list = vec![
            Action::new("Refresh all repositories", "status", Command::Refresh),
            Action::new("Edit branch rules", "settings", Command::Settings(Section::BranchRules)),
            Action::new("Open activity history", "settings", Command::Settings(Section::History)),
        ];
        assert_eq!(rank(&list, ""), vec![0, 1, 2]);
        assert_eq!(rank(&list, "  RULES "), vec![1]);
        assert_eq!(rank(&list, "activ"), vec![2]);
        assert_eq!(rank(&list, "settings"), vec![1, 2], "ties keep their order");
        assert!(rank(&list, "zzz").is_empty());
        assert_eq!(rank(&list, "rfsh"), vec![0], "scattered letters still match");
    }

    #[test]
    fn every_pull_request_opens_and_the_globals_are_always_there() {
        let state = demo();
        let list = actions(&state, |_| false);
        for pr in &state.pull_requests {
            assert!(has(&list, |c| matches!(c, Command::OpenPage { number, .. } if *number == pr.pr.number)));
            assert!(has(&list, |c| matches!(c, Command::OpenUrl(url) if *url == pr.pr.url)));
        }
        assert!(has(&list, |c| *c == Command::Refresh));
        for section in [Section::Repositories, Section::Commands, Section::BranchRules, Section::History, Section::General]
        {
            assert!(has(&list, |c| *c == Command::Settings(section)));
        }
        let none = actions(&ProductState::default(), |_| false);
        assert_eq!(none.len(), 6, "refresh and five settings sections");
    }

    #[test]
    fn running_needs_a_checkout_and_checking_out_needs_none() {
        let state = demo();
        let away = actions(&state, |_| false);
        let here = actions(&state, |_| true);
        assert!(!has(&away, |c| matches!(c, Command::Run { .. })), "not checked out: nothing to run");
        assert!(has(&away, |c| matches!(c, Command::CheckOut { .. })));
        assert!(has(&here, |c| matches!(c, Command::Run { .. })));
        assert!(!has(&here, |c| matches!(c, Command::CheckOut { .. })), "already at head");
    }

    #[test]
    fn only_signoffs_still_to_do_get_run_or_copy_actions() {
        let state = demo();
        let here = actions(&state, |_| true);
        for action in &here {
            match &action.command {
                Command::Run { repo_id, number, check } => {
                    let pr = state.pull_request(repo_id, *number).expect("pr");
                    let found = glance::required(pr).find(|c| &c.name == check).expect("check");
                    assert!(found.configured && found.state != CheckState::Success, "{}", action.label);
                }
                Command::Copy(command) => {
                    assert!(
                        state.pull_requests.iter().any(|pr| glance::required(pr)
                            .any(|c| &c.command == command && c.state != CheckState::Success)),
                        "{command}"
                    );
                }
                _ => {}
            }
        }
    }

    #[test]
    fn an_unconfigured_check_can_be_copied_but_not_run() {
        let mut state = demo();
        for pr in &mut state.pull_requests {
            for check in &mut pr.checks {
                check.configured = false;
            }
        }
        let list = actions(&state, |_| true);
        assert!(!has(&list, |c| matches!(c, Command::Run { .. } | Command::CheckOut { .. })));
        assert!(has(&list, |c| matches!(c, Command::Copy(_))));
    }
}
