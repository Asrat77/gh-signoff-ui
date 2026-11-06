//! What is worth a desktop notification, decided from two states.
//!
//! [`transitions`] compares the state the app showed before with the one it
//! got now and names what changed for the person: one of their pull
//! requests started failing, or became ready to merge. Delivery is the
//! app's; this is plain data so the rules are tested without a desktop.

use crate::product::{Health, ProductState, PullRequestState};

/// What changed on one of your pull requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// A required check failed.
    Failing,
    /// Every required check passed.
    Ready,
}

/// One thing to tell the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transition {
    /// The tracked repository's id.
    pub repo_id: String,
    /// `owner/name`.
    pub repo: String,
    /// The pull request.
    pub number: u64,
    /// Its title.
    pub title: String,
    /// What happened.
    pub change: Change,
}

impl Transition {
    /// The notification's title.
    pub fn headline(&self) -> String {
        match self.change {
            Change::Failing => format!("#{} is failing", self.number),
            Change::Ready => format!("#{} is ready to merge", self.number),
        }
    }

    /// The notification's body.
    pub fn detail(&self) -> String {
        format!("{} in {}", self.title, self.repo)
    }
}

/// The notifications `next` earns after `previous`.
///
/// Only pull requests whose author is `login` count, and only ones present
/// and fully read in both states, so the first state (`previous` is `None`
/// or has not read anything yet), a pull request that just appeared or
/// disappeared and a repository still loading say nothing. Each pull
/// request yields at most one transition per call, and a state compared
/// with itself yields none, so a repeat read never repeats a notification.
pub fn transitions(previous: Option<&ProductState>, next: &ProductState, login: &str) -> Vec<Transition> {
    let Some(previous) = previous.filter(|state| state.fetched_at.is_some()) else { return Vec::new() };
    if login.is_empty() || next.demo {
        return Vec::new();
    }
    next.pull_requests
        .iter()
        .filter(|now| now.pr.author == login && now.error.is_none() && !loading(next, now))
        .filter_map(|now| {
            let before = previous.pull_request(&now.repo_id, now.pr.number)?;
            if before.error.is_some() {
                return None;
            }
            change(before, now).map(|change| Transition {
                repo_id: now.repo_id.clone(),
                repo: now.repo.clone(),
                number: now.pr.number,
                title: now.pr.title.clone(),
                change,
            })
        })
        .collect()
}

fn change(before: &PullRequestState, now: &PullRequestState) -> Option<Change> {
    if now.status == Health::Failing && before.status != Health::Failing {
        Some(Change::Failing)
    } else if now.ready && now.total > 0 && !before.ready {
        Some(Change::Ready)
    } else {
        None
    }
}

fn loading(state: &ProductState, pr: &PullRequestState) -> bool {
    state.repositories.iter().any(|repo| repo.id == pr.repo_id && repo.loading)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::RepositoryState;
    use jiff::Timestamp;

    fn pr(number: u64, author: &str, status: Health, ready: bool) -> PullRequestState {
        let mut pr = PullRequestState {
            id: format!("r1:{number}"),
            repo_id: "r1".into(),
            repo: "acme/widgets".into(),
            status,
            ready,
            total: 2,
            ..PullRequestState::default()
        };
        pr.pr.number = number;
        pr.pr.author = author.into();
        pr.pr.title = format!("Change {number}");
        pr
    }

    fn state(prs: Vec<PullRequestState>) -> ProductState {
        ProductState {
            fetched_at: Some(Timestamp::UNIX_EPOCH),
            pull_requests: prs,
            repositories: vec![RepositoryState { id: "r1".into(), ..RepositoryState::default() }],
            ..ProductState::default()
        }
    }

    fn changes(previous: &ProductState, next: &ProductState) -> Vec<(u64, Change)> {
        transitions(Some(previous), next, "me").into_iter().map(|t| (t.number, t.change)).collect()
    }

    #[test]
    fn your_pull_request_starting_to_fail_is_reported_once() {
        let before = state(vec![pr(1, "me", Health::Attention, false)]);
        let after = state(vec![pr(1, "me", Health::Failing, false)]);
        assert_eq!(changes(&before, &after), [(1, Change::Failing)]);
        assert!(changes(&after, &after).is_empty(), "a repeat read says nothing");
    }

    #[test]
    fn your_pull_request_becoming_ready_is_reported() {
        let before = state(vec![pr(1, "me", Health::Attention, false)]);
        let after = state(vec![pr(1, "me", Health::Ready, true)]);
        let found = transitions(Some(&before), &after, "me");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].headline(), "#1 is ready to merge");
        assert_eq!(found[0].detail(), "Change 1 in acme/widgets");
        assert!(changes(&after, &after).is_empty());
    }

    #[test]
    fn failing_then_fixed_straight_to_ready_reports_ready() {
        let before = state(vec![pr(1, "me", Health::Failing, false)]);
        let after = state(vec![pr(1, "me", Health::Ready, true)]);
        assert_eq!(changes(&before, &after), [(1, Change::Ready)]);
    }

    #[test]
    fn other_peoples_pull_requests_are_not_yours() {
        let before = state(vec![pr(1, "them", Health::Attention, false)]);
        let after = state(vec![pr(1, "them", Health::Failing, false)]);
        assert!(changes(&before, &after).is_empty());
        assert!(transitions(Some(&before), &after, "").is_empty(), "no login, no owner");
    }

    #[test]
    fn the_first_state_and_unread_states_say_nothing() {
        let after = state(vec![pr(1, "me", Health::Failing, false)]);
        assert!(transitions(None, &after, "me").is_empty());
        let unread = ProductState { fetched_at: None, ..ProductState::default() };
        assert!(transitions(Some(&unread), &after, "me").is_empty(), "startup is not a transition");
    }

    #[test]
    fn appearing_and_disappearing_pull_requests_say_nothing() {
        let before = state(vec![pr(1, "me", Health::Attention, false)]);
        let appeared = state(vec![pr(1, "me", Health::Attention, false), pr(2, "me", Health::Failing, false)]);
        assert!(changes(&before, &appeared).is_empty(), "a new pull request is not a change");
        let gone = state(Vec::new());
        assert!(changes(&before, &gone).is_empty());
    }

    #[test]
    fn unreadable_pull_requests_and_loading_repositories_say_nothing() {
        let before = state(vec![pr(1, "me", Health::Attention, false)]);
        let mut broken = pr(1, "me", Health::Failing, false);
        broken.error = Some("could not read".into());
        assert!(changes(&before, &state(vec![broken.clone()])).is_empty(), "a read error is not a failing check");
        assert!(changes(&state(vec![broken]), &state(vec![pr(1, "me", Health::Ready, true)])).is_empty());

        let mut loading = state(vec![pr(1, "me", Health::Failing, false)]);
        loading.repositories[0].loading = true;
        assert!(changes(&before, &loading).is_empty());
    }

    #[test]
    fn demo_data_never_notifies() {
        let before = state(vec![pr(1, "me", Health::Attention, false)]);
        let mut after = state(vec![pr(1, "me", Health::Failing, false)]);
        after.demo = true;
        assert!(changes(&before, &after).is_empty());
    }

    #[test]
    fn a_pull_request_without_required_checks_is_not_ready() {
        let before = state(vec![pr(1, "me", Health::Neutral, false)]);
        let mut empty = pr(1, "me", Health::Neutral, true);
        empty.total = 0;
        assert!(changes(&before, &state(vec![empty])).is_empty());
    }
}
