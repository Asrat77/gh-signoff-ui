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

/// How many pull requests an empty query lists before the app actions.
const PREVIEW: usize = 4;

/// What the palette lists first: one entry per pull request, then the
/// actions of the app itself. The pull request being viewed comes first,
/// then `login`'s own, then the rest in the state's order. Enter opens a
/// pull request; [`pull_request_actions`] holds what else can be done to it.
pub(crate) fn actions(state: &ProductState, login: &str, current: Option<(&str, u64)>) -> Vec<Action> {
    let mut prs: Vec<&PullRequestState> = state.pull_requests.iter().collect();
    // Stable, so each group keeps the state's order.
    prs.sort_by_key(|pr| {
        let viewing = current.is_some_and(|(repo, number)| repo == pr.repo_id && number == pr.pr.number);
        (!viewing, !glance::is_yours(pr, login))
    });
    let mut out: Vec<Action> = prs.into_iter().map(open_page).collect();
    out.push(Action::new("Refresh all repositories", "status", Command::Refresh));
    out.push(Action::new("Add a repository", "settings", Command::Settings(Section::Repositories)));
    for (label, section) in [
        ("Manage repositories", Section::Repositories),
        ("Configure commands", Section::Commands),
        ("Edit branch rules", Section::BranchRules),
        ("Open activity history", Section::History),
        ("Open settings", Section::General),
    ] {
        out.push(Action::new(label, "settings", Command::Settings(section)));
    }
    out
}

fn open_page(pr: &PullRequestState) -> Action {
    Action::new(
        format!("#{} {}", pr.pr.number, pr.pr.title),
        pr.repo.clone(),
        Command::OpenPage { repo_id: pr.repo_id.clone(), number: pr.pr.number },
    )
}

/// What can be done to one pull request: open it, run what is left to
/// sign off, check it out, copy the commands, or open it on GitHub.
///
/// `at_head` says whether its checkout is on its head commit (the demo
/// pretends so after a simulated checkout). `isolated` says whether a run
/// elsewhere goes to Signoff's own worktree, so running needs no checkout
/// and checking out comes after it.
pub(crate) fn pull_request_actions(pr: &PullRequestState, at_head: bool, isolated: bool) -> Vec<Action> {
    let (repo_id, number) = (pr.repo_id.clone(), pr.pr.number);
    let mut out = vec![open_page(pr)];
    let open: Vec<&Check> = glance::required(pr).filter(|check| check.state != CheckState::Success).collect();
    if at_head || isolated {
        for check in open.iter().filter(|check| check.configured) {
            out.push(Action::new(
                format!("Run & sign off {}", check.name),
                format!("{} #{number}", pr.repo),
                Command::Run { repo_id: repo_id.clone(), number, check: check.name.clone() },
            ));
        }
    }
    if !at_head && open.iter().any(|check| check.configured) && !pr.repo_path.as_os_str().is_empty() {
        out.push(Action::new(
            "Check out this pull request",
            format!("switches {} to {}", pr.repo, pr.pr.head_ref),
            Command::CheckOut { repo_id, number, path: pr.repo_path.clone() },
        ));
    }
    for check in open {
        out.push(Action::new(
            format!("Copy {}", check.command),
            format!("{} #{number}", pr.repo),
            Command::Copy(check.command.clone()),
        ));
    }
    out.push(Action::new("Open on GitHub", pr.repo.clone(), Command::OpenUrl(pr.pr.url.clone())));
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

/// How well one word of the query matches an action: the pull request's
/// number exactly, then the start of a word, then anywhere inside, and
/// only for an app action (never a pull request's title) scattered letters.
fn word_score(action: &Action, word: &str) -> Option<i32> {
    let label = action.label.to_lowercase();
    let text = format!("{label} {}", action.meta.to_lowercase());
    let number = word.strip_prefix('#').unwrap_or(word);
    let is_number = !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit());
    if is_number
        && matches!(action.command, Command::OpenPage { .. })
        && label.strip_prefix('#').and_then(|rest| rest.split(' ').next()) == Some(number)
    {
        return Some(3000);
    }
    let at_word = text
        .match_indices(word)
        .find(|(at, _)| text[..*at].chars().next_back().is_none_or(|c| !c.is_alphanumeric()))
        .map(|(at, _)| at);
    if let Some(at) = at_word {
        return Some(2000 - i32::try_from(at).unwrap_or(i32::MAX));
    }
    if let Some(at) = text.find(word) {
        return Some(1000 - i32::try_from(at).unwrap_or(i32::MAX));
    }
    let app_action = !matches!(action.command, Command::OpenPage { .. });
    if app_action && word.chars().count() >= 3 { fuzzy_score(&text, word) } else { None }
}

/// The indexes of `actions` that match `query`, best first; equal scores
/// keep their order, so the pull requests listed first stay first. Every
/// word of the query must match. An empty query lists the first few pull
/// requests and then everything else, in order.
pub(crate) fn rank(actions: &[Action], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        let mut pulls = 0;
        return (0..actions.len())
            .filter(|&at| {
                if !matches!(actions[at].command, Command::OpenPage { .. }) {
                    return true;
                }
                pulls += 1;
                pulls <= PREVIEW
            })
            .collect();
    }
    let words: Vec<&str> = query.split_whitespace().collect();
    let mut scored: Vec<(usize, i32)> = actions
        .iter()
        .enumerate()
        .filter_map(|(order, action)| {
            let mut total = 0;
            for word in &words {
                total += word_score(action, word)?;
            }
            Some((order, total))
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

    fn pull_request(state: &ProductState, number: u64) -> &PullRequestState {
        state.pull_requests.iter().find(|pr| pr.pr.number == number).expect("pr")
    }

    #[test]
    fn each_pull_request_has_one_entry_and_the_app_actions_follow() {
        let state = demo();
        let list = actions(&state, "", None);
        let opens = list.iter().filter(|a| matches!(a.command, Command::OpenPage { .. })).count();
        assert_eq!(opens, state.pull_requests.len());
        assert_eq!(list.len(), opens + 7, "refresh, add a repository and five settings sections");
        assert!(has(&list, |c| *c == Command::Refresh));
        assert!(has(&list, |c| *c == Command::Settings(Section::Repositories)));
        for section in
            [Section::Repositories, Section::Commands, Section::BranchRules, Section::History, Section::General]
        {
            assert!(has(&list, |c| *c == Command::Settings(section)));
        }
        let none = actions(&ProductState::default(), "", None);
        assert_eq!(none.len(), 7);
    }

    #[test]
    fn the_pull_request_being_viewed_comes_first_then_yours() {
        let state = demo();
        let other = state.pull_requests.last().expect("a pull request");
        let (repo, number) = (other.repo_id.as_str(), other.pr.number);
        let list = actions(&state, "", Some((repo, number)));
        assert!(matches!(&list[0].command, Command::OpenPage { number: n, .. } if *n == number));
        let author = state.pull_requests[1].pr.author.clone();
        let list = actions(&state, &author, None);
        let first = list.iter().find_map(|a| match &a.command {
            Command::OpenPage { repo_id, number } => state.pull_request(repo_id, *number),
            _ => None,
        });
        assert!(first.is_some_and(|pr| pr.pr.author.eq_ignore_ascii_case(&author)));
    }

    #[test]
    fn an_empty_query_lists_a_few_pull_requests_then_the_app_actions() {
        let state = demo();
        let list = actions(&state, "", None);
        let shown = rank(&list, "");
        let opens = shown.iter().filter(|at| matches!(list[**at].command, Command::OpenPage { .. })).count();
        assert_eq!(opens, PREVIEW.min(state.pull_requests.len()));
        assert!(shown.iter().any(|at| list[*at].command == Command::Refresh));
    }

    #[test]
    fn a_number_or_a_title_word_beats_loose_matches() {
        let state = demo();
        let list = actions(&state, "", None);
        let number = state.pull_requests[0].pr.number;
        let top = rank(&list, &format!("#{number}"))[0];
        assert!(matches!(&list[top].command, Command::OpenPage { number: n, .. } if *n == number));
        let top = rank(&list, "poller")[0];
        assert!(list[top].label.contains("poller"));
        let found = rank(&list, "poller");
        assert!(
            found.iter().all(|at| list[*at].label.to_lowercase().contains("poller")
                || list[*at].meta.to_lowercase().contains("poller"))
        );
    }

    #[test]
    fn scattered_letters_never_match_a_pull_request() {
        let list = vec![
            Action::new(
                "#2 build(deps): bump actions/checkout from 4 to 5",
                "bella_sports",
                Command::OpenPage { repo_id: "r".into(), number: 2 },
            ),
            Action::new("Refresh all repositories", "status", Command::Refresh),
        ];
        assert!(rank(&list, "mock").is_empty(), "m-o-c-k are all in there, in order");
        assert_eq!(rank(&list, "rfsh"), vec![1], "an app action still matches loosely");
    }

    #[test]
    fn every_word_of_the_query_must_match() {
        let list = vec![Action::new(
            "#7 Fix flaky mock server",
            "acme/api",
            Command::OpenPage { repo_id: "r".into(), number: 7 },
        )];
        assert_eq!(rank(&list, "mock acme"), vec![0]);
        assert!(rank(&list, "mock nope").is_empty());
    }

    #[test]
    fn a_pull_request_offers_open_run_check_out_copy_and_github() {
        let state = demo();
        let pr = pull_request(&state, state.pull_requests[0].pr.number);
        let away = pull_request_actions(pr, false, false);
        assert!(matches!(away[0].command, Command::OpenPage { .. }));
        assert!(!has(&away, |c| matches!(c, Command::Run { .. })), "not checked out: nothing to run");
        assert!(has(&away, |c| matches!(c, Command::CheckOut { .. })));
        assert!(has(&away, |c| matches!(c, Command::OpenUrl(url) if *url == pr.pr.url)));
        let here = pull_request_actions(pr, true, false);
        assert!(has(&here, |c| matches!(c, Command::Run { .. })));
        assert!(!has(&here, |c| matches!(c, Command::CheckOut { .. })), "already at head");
    }

    #[test]
    fn with_a_separate_worktree_running_needs_no_checkout_and_comes_first() {
        let state = demo();
        let actions = state.pull_requests.iter().find_map(|pr| {
            let list = pull_request_actions(pr, false, true);
            list.iter().any(|a| matches!(a.command, Command::CheckOut { .. })).then_some(list)
        });
        let away = actions.expect("a pull request to check out");
        let run = away.iter().position(|a| matches!(a.command, Command::Run { .. })).expect("a run");
        let check_out = away.iter().position(|a| matches!(a.command, Command::CheckOut { .. })).expect("a checkout");
        assert!(run < check_out, "running is the main action");
        assert!(away[check_out].meta.starts_with("switches "), "{}", away[check_out].meta);
    }

    #[test]
    fn only_signoffs_still_to_do_get_run_or_copy_actions() {
        let state = demo();
        for pr in &state.pull_requests {
            for action in pull_request_actions(pr, true, false) {
                match &action.command {
                    Command::Run { check, .. } => {
                        let found = glance::required(pr).find(|c| &c.name == check).expect("check");
                        assert!(found.configured && found.state != CheckState::Success, "{}", action.label);
                    }
                    Command::Copy(command) => {
                        assert!(
                            glance::required(pr).any(|c| &c.command == command && c.state != CheckState::Success),
                            "{command}"
                        );
                    }
                    _ => {}
                }
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
        let all: Vec<Action> =
            state.pull_requests.iter().flat_map(|pr| pull_request_actions(pr, true, false)).collect();
        assert!(!has(&all, |c| matches!(c, Command::Run { .. } | Command::CheckOut { .. })));
        assert!(has(&all, |c| matches!(c, Command::Copy(_))));
    }
}
