//! What the glance popover shows, worked out from the state with no UI.
//!
//! The popover draws these values with signoff-ui's widgets; keeping the
//! decisions here (which pull requests a filter keeps, what the headline
//! says, which check needs you) means they are tested without a window.

use jiff::Timestamp;
use signoff_core::{Check, CheckState, Health, ProductState, PullRequestState};
use signoff_ui::Status;
use signoff_ui::widgets::Segment;

/// The design system's state for a health.
pub(crate) fn status(health: Health) -> Status {
    match health {
        Health::Failing => Status::Failing,
        Health::Attention => Status::Attention,
        Health::Ready => Status::Ready,
        Health::Neutral => Status::Neutral,
    }
}

/// The chips above the list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Filter {
    /// Pull requests the signed-in user opened.
    #[default]
    Yours,
    /// Every pull request the tracked repositories show.
    All,
    /// Only failing ones.
    Failing,
    /// Only ready ones.
    Ready,
}

impl Filter {
    /// Every filter, in the order the chips show.
    pub(crate) const ALL: [Self; 4] = [Self::Yours, Self::All, Self::Failing, Self::Ready];

    /// The chip's label.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Yours => "Yours",
            Self::All => "All",
            Self::Failing => "Failing",
            Self::Ready => "Ready",
        }
    }

    /// Whether `pr` shows under this filter. `login` is the signed-in
    /// user; with none known, every pull request counts as yours.
    pub(crate) fn keeps(self, pr: &PullRequestState, login: &str) -> bool {
        match self {
            Self::Yours => is_yours(pr, login),
            Self::All => true,
            Self::Failing => pr.status == Health::Failing,
            Self::Ready => pr.status == Health::Ready,
        }
    }
}

/// Whether `login` opened `pr`.
pub(crate) fn is_yours(pr: &PullRequestState, login: &str) -> bool {
    login.is_empty() || pr.pr.author.eq_ignore_ascii_case(login)
}

/// The pull requests `filter` keeps, in the state's order: failing, then
/// needing attention, then the rest, most recently updated first.
pub(crate) fn visible(state: &ProductState, filter: Filter) -> Vec<&PullRequestState> {
    state.pull_requests.iter().filter(|pr| filter.keeps(pr, &state.env.login)).collect()
}

/// How many pull requests each filter keeps.
pub(crate) fn count(state: &ProductState, filter: Filter) -> usize {
    state.pull_requests.iter().filter(|pr| filter.keeps(pr, &state.env.login)).count()
}

/// The big line at the top of the list and the line under it.
pub(crate) fn headline(state: &ProductState) -> (String, String) {
    let summary = &state.summary;
    let failing = summary.failing;
    let waiting = summary.needs_attention - failing;
    if summary.needs_attention > 0 {
        let title = match summary.needs_attention {
            1 => "1 PR needs you".to_owned(),
            n => format!("{n} PRs need you"),
        };
        let mut parts = Vec::new();
        if failing > 0 {
            parts.push(format!("{failing} failing"));
        }
        if waiting > 0 {
            parts.push(format!("{waiting} waiting on your signoff"));
        }
        (title, parts.join(" · "))
    } else if summary.ready > 0 {
        let title = format!("{} ready to merge", summary.ready);
        let detail = if summary.ready == summary.total_pull_requests {
            "Every required signoff passes on head.".to_owned()
        } else {
            let other = summary.total_pull_requests - summary.ready;
            format!("{other} more with no signoffs required.")
        };
        (title, detail)
    } else {
        ("Nothing to sign off".to_owned(), "No open pull request needs a signoff.".to_owned())
    }
}

/// The pull request the popover should send you to first, if any: the
/// first failing or waiting one under `filter`.
pub(crate) fn next_up(state: &ProductState, filter: Filter) -> Option<&PullRequestState> {
    visible(state, filter).into_iter().find(|pr| matches!(pr.status, Health::Failing | Health::Attention))
}

/// The checks a pull request's merge waits on, in order.
pub(crate) fn required(pr: &PullRequestState) -> impl Iterator<Item = &Check> {
    pr.checks.iter().filter(|check| check.required)
}

/// The small progress bar at the end of a pull request's row.
pub(crate) fn row_segments(pr: &PullRequestState) -> Vec<Segment> {
    required(pr)
        .map(|check| match check.state {
            CheckState::Success => Segment::Done,
            CheckState::Failure | CheckState::Error => Segment::Current(Status::Failing),
            CheckState::Pending | CheckState::Absent => Segment::Empty,
        })
        .collect()
}

/// "Updated 12s ago", for the foot of the list.
pub(crate) fn updated(fetched_at: Option<Timestamp>, now: Timestamp) -> String {
    let Some(at) = fetched_at else {
        return "Not updated yet".to_owned();
    };
    let seconds = now.duration_since(at).as_secs().max(0);
    match seconds {
        0..5 => "Updated just now".to_owned(),
        5..60 => format!("Updated {seconds}s ago"),
        60..3600 => format!("Updated {}m ago", seconds / 60),
        _ => format!("Updated {}h ago", seconds / 3600),
    }
}

#[cfg(test)]
mod tests {
    use jiff::SignedDuration;
    use signoff_core::demo;

    use super::*;

    fn demo_state() -> ProductState {
        demo::product_state(Timestamp::UNIX_EPOCH)
    }

    fn pr(author: &str, status: Health) -> PullRequestState {
        let mut pr = PullRequestState { status, ..PullRequestState::default() };
        pr.pr.author = author.into();
        pr
    }

    fn check(name: &str, state: CheckState) -> Check {
        Check { name: name.into(), state, required: true, ..Check::default() }
    }

    fn with_checks(checks: Vec<Check>) -> PullRequestState {
        let mut pr = PullRequestState { checks, ..PullRequestState::default() };
        pr.retally();
        pr
    }

    #[test]
    fn yours_keeps_only_the_signed_in_users_pull_requests() {
        assert!(Filter::Yours.keeps(&pr("Octocat", Health::Ready), "octocat"));
        assert!(!Filter::Yours.keeps(&pr("hubot", Health::Ready), "octocat"));
        assert!(Filter::All.keeps(&pr("hubot", Health::Ready), "octocat"));
        assert!(Filter::Yours.keeps(&pr("hubot", Health::Ready), ""), "no login known: everything is yours");
    }

    #[test]
    fn state_filters_keep_only_their_state() {
        assert!(Filter::Failing.keeps(&pr("a", Health::Failing), "a"));
        assert!(!Filter::Failing.keeps(&pr("a", Health::Attention), "a"));
        assert!(Filter::Ready.keeps(&pr("a", Health::Ready), "a"));
        assert!(!Filter::Ready.keeps(&pr("a", Health::Neutral), "a"));
    }

    #[test]
    fn the_list_keeps_failing_first_and_ready_last() {
        let state = demo_state();
        let order: Vec<Health> = visible(&state, Filter::All).iter().map(|pr| pr.status).collect();
        assert_eq!(order, [Health::Failing, Health::Attention, Health::Ready]);
        assert_eq!(count(&state, Filter::Failing), 1);
        assert_eq!(next_up(&state, Filter::All).map(|pr| pr.pr.number), Some(485));
    }

    #[test]
    fn the_headline_counts_what_needs_you() {
        let (title, detail) = headline(&demo_state());
        assert_eq!(title, "2 PRs need you");
        assert_eq!(detail, "1 failing · 1 waiting on your signoff");

        let mut state = ProductState::default();
        state.summary.total_pull_requests = 2;
        state.summary.ready = 2;
        assert_eq!(headline(&state).0, "2 ready to merge");
        state.summary.ready = 0;
        assert_eq!(headline(&state).0, "Nothing to sign off");
    }

    #[test]
    fn the_row_bar_marks_signed_and_failed_checks() {
        let pr = with_checks(vec![
            check("lint", CheckState::Success),
            check("security", CheckState::Absent),
            check("tests", CheckState::Failure),
        ]);
        assert_eq!(row_segments(&pr), [Segment::Done, Segment::Empty, Segment::Current(Status::Failing)]);
    }

    #[test]
    fn optional_checks_stay_off_the_bar() {
        let mut optional = check("docs", CheckState::Absent);
        optional.required = false;
        let pr = with_checks(vec![check("tests", CheckState::Success), optional]);
        assert_eq!(row_segments(&pr), [Segment::Done]);
    }

    #[test]
    fn updated_reads_like_a_person_would_say_it() {
        let now = Timestamp::UNIX_EPOCH + SignedDuration::from_hours(5);
        let ago = |secs: i64| Some(now - SignedDuration::from_secs(secs));
        assert_eq!(updated(None, now), "Not updated yet");
        assert_eq!(updated(ago(2), now), "Updated just now");
        assert_eq!(updated(ago(12), now), "Updated 12s ago");
        assert_eq!(updated(ago(150), now), "Updated 2m ago");
        assert_eq!(updated(ago(7300), now), "Updated 2h ago");
        assert_eq!(updated(Some(now + SignedDuration::from_secs(3)), now), "Updated just now", "clock skew");
    }
}
