//! Fixed sample data, for trying Signoff and building its UI without a
//! repository that has signoff protection. It is always labelled as demo
//! data, never reads or writes GitHub, and real mode never falls back to it.

use std::collections::BTreeMap;

use jiff::{SignedDuration, Timestamp};

use crate::checks::{Check, CheckState, Rules, build_install_command, category_for, signoff_command};
use crate::config::CommandMapping;
use crate::gh::{Environment, PrInfo};
use crate::history::{ActivityEvent, ActivityKind};
use crate::product::{ProductState, PullRequestState, RepositoryState, summarize};

/// The demo state at `now`: two repositories and three pull requests, one
/// failing, one needing attention, one ready.
pub fn product_state(now: Timestamp) -> ProductState {
    let ago = |minutes: i64| now - SignedDuration::from_mins(minutes);
    let commands = |names: &[&str]| -> BTreeMap<String, CommandMapping> {
        names
            .iter()
            .map(|name| ((*name).to_owned(), CommandMapping { command: format!("bin/{name}"), timeout_seconds: 1800 }))
            .collect()
    };
    let check = |name: &str, state: CheckState| -> Check {
        let context = format!("signoff/{name}");
        Check {
            name: name.to_owned(),
            category: category_for(name).to_owned(),
            state,
            required: true,
            command: signoff_command(&context),
            local_command: format!("bin/{name}"),
            configured: true,
            actor: if state == CheckState::Absent { String::new() } else { "you".into() },
            at: (state != CheckState::Absent).then(|| ago(4)),
            context,
            ..Check::default()
        }
    };
    let rules = |names: &[&str]| -> Rules {
        let contexts: Vec<String> = names.iter().map(|n| format!("signoff/{n}")).collect();
        Rules {
            branch: "main".into(),
            install_command: build_install_command(&contexts, "main"),
            signoff_contexts: contexts.clone(),
            contexts,
            readable: true,
            protected: true,
            enforced: true,
        }
    };
    let pr = |repo_id: &str,
              repo: &str,
              number: u64,
              title: &str,
              head: &str,
              branch: &str,
              mergeable: &str,
              updated: i64| {
        let info = PrInfo {
            number,
            title: title.into(),
            author: "you".into(),
            url: format!("https://github.com/{repo}/pull/{number}"),
            state: "OPEN".into(),
            head_sha: head.into(),
            head_ref: branch.into(),
            base_ref: "main".into(),
            mergeable: mergeable.into(),
            updated_at: ago(updated),
            is_draft: false,
            contexts: Vec::new(),
        };
        (repo_id.to_owned(), info)
    };

    let repositories = vec![
        RepositoryState {
            id: "demo-signoff".into(),
            path: "/demo/gh-signoff".into(),
            name_with_owner: "basecamp/gh-signoff".into(),
            default_branch: "main".into(),
            current_branch: "feature/partial-checks".into(),
            current_sha: "3db401c".into(),
            enabled: true,
            commands: commands(&["tests", "lint", "security"]),
            pull_request_count: 2,
            ..RepositoryState::default()
        },
        RepositoryState {
            id: "demo-rails".into(),
            path: "/demo/rails".into(),
            name_with_owner: "rails/rails".into(),
            default_branch: "main".into(),
            enabled: true,
            commands: commands(&["tests"]),
            pull_request_count: 1,
            ..RepositoryState::default()
        },
    ];

    let entries = [
        (
            pr(
                "demo-signoff",
                "basecamp/gh-signoff",
                482,
                "Add label-based partial signoff",
                "3db401c",
                "feature/partial-checks",
                "MERGEABLE",
                2,
            ),
            vec![
                check("tests", CheckState::Success),
                check("lint", CheckState::Success),
                check("security", CheckState::Absent),
            ],
            rules(&["tests", "lint", "security"]),
            true,
        ),
        (
            pr(
                "demo-signoff",
                "basecamp/gh-signoff",
                485,
                "Fix race condition in poller",
                "9f3a201",
                "bug/race-fix",
                "CONFLICTING",
                18,
            ),
            vec![check("tests", CheckState::Failure), check("lint", CheckState::Absent)],
            rules(&["tests", "lint"]),
            false,
        ),
        (
            pr("demo-rails", "rails/rails", 50122, "Bump version to 7.1.3", "9b110d2", "version-bump", "MERGEABLE", 30),
            vec![check("tests", CheckState::Success)],
            rules(&["tests"]),
            false,
        ),
    ];
    let mut pull_requests: Vec<PullRequestState> = entries
        .into_iter()
        .map(|((repo_id, info), checks, rules, checked_out)| {
            let repo = repositories
                .iter()
                .find(|r| r.id == repo_id)
                .map(|r| (r.name_with_owner.clone(), r.path.clone()))
                .unwrap_or_default();
            let mut state = PullRequestState {
                id: format!("{repo_id}:{}", info.number),
                repo_id,
                repo: repo.0,
                repo_path: repo.1,
                pr: info,
                checks,
                rules,
                checked_out,
                fetched_at: now,
                ..PullRequestState::default()
            };
            state.retally();
            state
        })
        .collect();
    pull_requests
        .sort_by(|a, b| a.status.rank().cmp(&b.status.rank()).then_with(|| b.pr.updated_at.cmp(&a.pr.updated_at)));

    let history = vec![
        ActivityEvent {
            id: "demo-1".into(),
            kind: ActivityKind::Status,
            repo_id: "demo-signoff".into(),
            repo: "basecamp/gh-signoff".into(),
            pr_number: 482,
            pr_title: "Add label-based partial signoff".into(),
            check: "lint".into(),
            state: CheckState::Success,
            actor: "you".into(),
            at: ago(4),
            ..ActivityEvent::default()
        },
        ActivityEvent {
            id: "demo-2".into(),
            kind: ActivityKind::Run,
            repo_id: "demo-signoff".into(),
            repo: "basecamp/gh-signoff".into(),
            pr_number: 485,
            pr_title: "Fix race condition in poller".into(),
            check: "tests".into(),
            state: CheckState::Failure,
            command: "bin/test".into(),
            exit_code: 1,
            detail: "1 failing test".into(),
            at: ago(22),
            output: vec![
                "$ bin/test".into(),
                "Running 214 tests across 18 files".into(),
                "\u{1b}[31mFAIL\u{1b}[0m poller_test.rb:88 drains the queue before stopping".into(),
                "  expected 0 jobs left, got 1".into(),
                "213 passed, 1 failed in 41.2s".into(),
            ],
            ..ActivityEvent::default()
        },
    ];

    ProductState {
        env: Environment {
            gh_installed: true,
            gh_version: "2.95.0".into(),
            authenticated: true,
            login: "you".into(),
            in_git_repo: true,
            signoff_installed: true,
            signoff_version: "0.3.0".into(),
        },
        summary: summarize(&pull_requests),
        repositories,
        pull_requests,
        history,
        config_path: "/demo/Signoff/config.json".into(),
        onboarded: true,
        demo: true,
        warning: Some("Demo data: nothing here is read from or written to GitHub.".into()),
        fetched_at: Some(now),
        ..ProductState::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::Health;

    #[test]
    fn covers_every_popover_state() {
        let state = product_state(Timestamp::now());
        assert!(state.demo);
        assert!(state.repositories.len() >= 2 && state.pull_requests.len() >= 3);
        let statuses: Vec<Health> = state.pull_requests.iter().map(|pr| pr.status).collect();
        assert_eq!(statuses, [Health::Failing, Health::Attention, Health::Ready], "failing first, ready last");
        assert_eq!(state.summary.total_pull_requests, 3);
        assert_eq!(state.summary.status, Health::Failing);
    }
}
