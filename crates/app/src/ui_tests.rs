//! UI tests: the real popover and settings, driven through their
//! accessibility tree with egui_kittest. No window, GPU or GitHub is needed;
//! `gh` and `git` are answered by [`Script`].

use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use signoff_core::config::{AppConfig, ConfigStore, TrackedRepository};
use signoff_core::{Gh, HistoryStore, Output, ProductService, ProductState, Runner};

use crate::app::Signoff;
use crate::settings::{self, Settings};
use crate::{popover, worker};

/// Answers `gh` and `git` by the first matching needle and records every
/// call with its stdin. Anything unmatched fails, as a missing command would.
#[derive(Default)]
struct Script {
    replies: Vec<(&'static str, String)>,
    calls: Mutex<Vec<(String, Option<String>)>>,
}

impl Script {
    fn reply(mut self, needle: &'static str, stdout: impl Into<String>) -> Self {
        self.replies.push((needle, stdout.into()));
        self
    }

    fn calls(&self) -> Vec<(String, Option<String>)> {
        self.calls.lock().map(|calls| calls.clone()).unwrap_or_default()
    }
}

impl Runner for Script {
    fn run(&self, _dir: Option<&Path>, input: Option<&str>, program: &str, args: &[&str]) -> io::Result<Output> {
        let line = format!("{program} {}", args.join(" "));
        if let Ok(mut calls) = self.calls.lock() {
            calls.push((line.clone(), input.map(str::to_owned)));
        }
        Ok(match self.replies.iter().find(|(needle, _)| line.contains(needle)) {
            Some((_, stdout)) => Output { stdout: stdout.clone(), stderr: String::new(), code: 0 },
            None => Output { stdout: String::new(), stderr: format!("not scripted: {line}"), code: 1 },
        })
    }
}

/// A service on a temporary config holding `repos`, reading GitHub through
/// `script`.
fn service(dir: &Path, repos: Vec<TrackedRepository>, script: Arc<Script>) -> Arc<ProductService> {
    let config =
        ConfigStore::with_config(dir.join("config.json"), AppConfig { repositories: repos, ..AppConfig::default() });
    let history = HistoryStore::open(dir.join("history.json")).expect("history");
    Arc::new(ProductService::new(Gh::new(script), Arc::new(config), Arc::new(history), None))
}

fn tracked(path: &Path) -> TrackedRepository {
    TrackedRepository {
        id: "repo1".into(),
        path: path.to_owned(),
        name_with_owner: "acme/widgets".into(),
        default_branch: "main".into(),
        enabled: true,
        ..TrackedRepository::default()
    }
}

/// What a settings harness draws.
struct SettingsUnderTest {
    settings: Settings,
    state: ProductState,
    service: Arc<ProductService>,
    refreshes: usize,
    /// Read the state again on refresh, as the app does. Off, the state
    /// stays as it was: a refresh that has not finished yet.
    follow: bool,
}

fn settings_harness(service: Arc<ProductService>, state: ProductState) -> Harness<'static, SettingsUnderTest> {
    let settings = Settings::new(service.clone(), || {});
    let under_test = SettingsUnderTest { settings, state, service, refreshes: 0, follow: true };
    Harness::builder().with_size(egui::vec2(820.0, 900.0)).build_ui_state(
        |ui, t: &mut SettingsUnderTest| {
            if !installed(ui.ctx()) {
                return;
            }
            if settings::contents(ui, &mut t.settings, &t.state).refresh {
                t.refreshes += 1;
                // What the app does on refresh: read the state again.
                if t.follow {
                    t.state = t.service.cached_or_config_state();
                }
            }
        },
        under_test,
    )
}

/// Sets up fonts and palettes once per context. Fonts apply from the next
/// frame, so the first frame draws nothing: false until then.
fn installed(ctx: &egui::Context) -> bool {
    // As the real shell does: a durable file write while drawing panics.
    fastframe_file::mark_ui_thread();
    let id = egui::Id::new("ui-tests-installed");
    if ctx.data(|d| d.get_temp::<bool>(id).unwrap_or(false)) {
        return true;
    }
    signoff_ui::install(ctx, signoff_ui::ThemeMode::Dark);
    ctx.data_mut(|d| d.insert_temp(id, true));
    ctx.request_repaint();
    false
}

/// Steps the harness until `done` holds, for work on background threads.
fn step_until<S>(harness: &mut Harness<'_, S>, what: &str, mut done: impl FnMut(&Harness<'_, S>) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(harness) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
        harness.step();
    }
    harness.run_ok();
}

/// Types `text` into the `index`th text field, after focusing it.
fn type_into<S>(harness: &mut Harness<'_, S>, index: usize, text: &str) {
    let field = |h: &Harness<'_, S>| {
        h.get_all_by_role(egui::accesskit::Role::TextInput)
            .nth(index)
            .map(|node| node.accesskit_node().id())
            .expect("a text field")
    };
    let id = field(harness);
    harness.get_by(|node| node.id() == id).focus();
    harness.run_ok();
    harness.get_by(|node| node.id() == id).type_text(text);
    harness.run_ok();
}

trait StateFromConfig {
    fn cached_or_config_state(&self) -> ProductState;
}

impl StateFromConfig for ProductService {
    /// The repositories in the config, as a refresh would list them,
    /// without reading pull requests.
    fn cached_or_config_state(&self) -> ProductState {
        let mut state = self.cached();
        if let Some(config) = self.config() {
            state.repositories = config
                .snapshot()
                .repositories
                .into_iter()
                .map(|repo| signoff_core::RepositoryState {
                    id: repo.id,
                    path: repo.path,
                    name_with_owner: repo.name_with_owner,
                    default_branch: repo.default_branch,
                    enabled: repo.enabled,
                    pull_request_scope: repo.pull_request_scope,
                    commands: repo.commands,
                    ..signoff_core::RepositoryState::default()
                })
                .collect();
        }
        state
    }
}

fn demo_state() -> (Arc<ProductService>, ProductState) {
    let service = Arc::new(ProductService::demo());
    let state = service.refresh();
    (service, state)
}

#[test]
fn settings_covers_every_section() {
    let (service, state) = demo_state();
    let mut harness = settings_harness(service, state);
    harness.run_ok();
    harness.get_by_label("Launch at login");
    let sections = [
        ("Repositories", "basecamp/gh-signoff"),
        (
            "Commands",
            "What runs on your computer before Signoff signs off a check. Each runs in the checkout, on the pull request's head commit, and the signoff is written only when it exits 0.",
        ),
        ("Branch rules", "Required signoff checks"),
        ("History", "History"),
        ("General", "Launch at login"),
    ];
    for (section, shows) in sections {
        harness.get_by_label(section).click();
        harness.run_ok();
        assert!(harness.query_all_by_label_contains(shows).next().is_some(), "{section} should show {shows}");
    }
}

#[test]
fn demo_settings_save_nothing() {
    let (service, state) = demo_state();
    let mut harness = settings_harness(service, state);
    harness.run_ok();
    harness.get_by_label_contains("Demo data: settings are shown but nothing is saved");
    harness.get_by_label("Repositories").click();
    harness.run_ok();
    assert!(harness.query_by_label("Add a local checkout").is_none());
}

#[test]
fn adding_a_checkout_tracks_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let checkout = dir.path().join("widgets");
    std::fs::create_dir(&checkout).expect("checkout");
    let script = Arc::new(
        Script::default()
            .reply("repo view", r#"{"nameWithOwner":"acme/widgets","defaultBranchRef":{"name":"main"}}"#)
            .reply("rev-parse", "main"),
    );
    let service = service(dir.path(), Vec::new(), script);
    let mut harness = settings_harness(service.clone(), ProductState::default());
    harness.run_ok();
    harness.get_by_label("Repositories").click();
    harness.run_ok();

    type_into(&mut harness, 0, &checkout.display().to_string());
    harness.get_by_label("Add").click();
    step_until(&mut harness, "the checkout to be added", |h| h.state().refreshes > 0);

    let repos = service.config().expect("config").snapshot().repositories;
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0].name_with_owner, "acme/widgets");
    assert!(harness.query_by_label("acme/widgets").is_some());
}

#[test]
fn a_repository_chip_changes_before_the_refresh_finishes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let service = service(dir.path(), vec![tracked(dir.path())], Arc::new(Script::default()));
    let state = service.cached_or_config_state();
    let mut harness = settings_harness(service.clone(), state);
    harness.state_mut().follow = false;
    harness.run_ok();
    harness.get_by_label("Repositories").click();
    harness.run_ok();

    harness.get_by_label("Watching").click();
    harness.run_ok();
    assert!(harness.query_by_label("Paused").is_some(), "the chip should flip at once");
    assert!(!service.config().expect("config").find_repository("repo1").expect("repo").enabled);

    harness.get_by_label("Stop tracking").click();
    harness.run_ok();
    harness.get_by_label("Remove").click();
    harness.run_ok();
    assert!(harness.query_by_label("acme/widgets").is_none(), "the row should go at once");
    assert!(service.config().expect("config").snapshot().repositories.is_empty());
}

#[test]
fn a_new_repository_reads_until_a_refresh_covers_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let service = service(dir.path(), vec![tracked(dir.path())], Arc::new(Script::default()));
    let mut harness = settings_harness(service, ProductState::default());
    harness.run_ok();
    harness.get_by_label("Repositories").click();
    harness.run_ok();
    harness.get_by_label("acme/widgets");
    harness.get_by_label("Reading…");
}

#[test]
fn a_command_is_saved_for_a_check() {
    let dir = tempfile::tempdir().expect("tempdir");
    let service = service(dir.path(), vec![tracked(dir.path())], Arc::new(Script::default()));
    let state = service.cached_or_config_state();
    let mut harness = settings_harness(service.clone(), state);
    harness.run_ok();
    harness.get_by_label("Commands").click();
    harness.run_ok();

    type_into(&mut harness, 0, "tests");
    harness.get_by_label("Add").click();
    harness.run_ok();

    type_into(&mut harness, 0, "cargo test --workspace");
    harness.get_by_label("Save commands").click();
    harness.run_ok();

    let repo = service.config().expect("config").find_repository("repo1").expect("repo");
    assert_eq!(repo.commands["tests"].command, "cargo test --workspace");
}

#[test]
fn branch_rules_change_only_signoff_checks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = Arc::new(
        Script::default()
            .reply("check-ref-format", "main")
            .reply("protection --jq", r#"["ci/build","signoff/tests"]"#)
            .reply("--method PUT", "{}"),
    );
    let service = service(dir.path(), vec![tracked(dir.path())], script.clone());
    let state = service.cached_or_config_state();
    let mut harness = settings_harness(service, state);
    harness.run_ok();
    harness.get_by_label("Branch rules").click();
    step_until(&mut harness, "the rules to load", |h| h.query_by_label("ci/build").is_some());
    harness.get_by_label("tests");

    // The branch field, then the field that adds a check.
    type_into(&mut harness, 1, "lint");
    harness.get_by_label("Add").click();
    harness.run_ok();
    harness.get_by_label("Save to GitHub").click();
    step_until(&mut harness, "the rules to be written", |_| {
        script.calls().iter().any(|(line, _)| line.contains("--method PUT"))
    });

    let calls = script.calls();
    let (line, input) = calls.iter().find(|(line, _)| line.contains("--method PUT")).expect("a write");
    assert_eq!(
        line,
        "gh api --method PUT repos/acme/widgets/branches/main/protection/required_status_checks/contexts --input -"
    );
    assert_eq!(input.as_deref(), Some(r#"{"contexts":["ci/build","signoff/lint","signoff/tests"]}"#));
}

/// A demo app, as `--demo` starts it, with the state already read.
fn demo_app() -> Signoff {
    let service = Arc::new(ProductService::demo());
    let settings = Settings::new(service.clone(), || {});
    let refresher = worker::Refresher::spawn(service.clone(), || {});
    let runs = crate::runs::Runs::new(Arc::new(signoff_core::Verifier::demo(service.clone())), || {});
    let mut app = Signoff::new(None, refresher, runs, settings, true);
    app.state = service.refresh();
    app
}

fn popover_harness() -> Harness<'static, Signoff> {
    Harness::builder().with_size(egui::vec2(380.0, 620.0)).build_ui_state(
        |ui, app: &mut Signoff| {
            if installed(ui.ctx()) {
                // As the window does each frame: take in new state, then draw.
                app.tick(Some(ui.ctx()));
                popover::show(ui, app);
            }
        },
        demo_app(),
    )
}

#[test]
fn the_glance_lists_pull_requests_across_repositories() {
    let mut harness = popover_harness();
    harness.run_ok();
    assert!(harness.query_by_label_contains("Add label-based partial signoff").is_some());
    assert!(harness.query_by_label_contains("Fix race condition in poller").is_some());
    harness.get_by_label("All").click();
    harness.run_ok();
    assert!(harness.query_by_label_contains("Bump version to 7.1.3").is_some());
}

#[test]
fn a_pull_request_opens_its_signoffs() {
    let mut harness = popover_harness();
    harness.run_ok();
    harness.get_by_label_contains("Add label-based partial signoff").click();
    harness.run_ok();
    assert!(harness.query_by_label_contains("security:").is_some());
    assert!(harness.query_by_label("Run & sign off security").is_some());
}

/// The Playwright suite's main path: run the last check and reach ready.
#[test]
fn running_the_last_check_makes_it_ready_to_merge() {
    let mut harness = popover_harness();
    harness.run_ok();
    harness.get_by_label_contains("Add label-based partial signoff").click();
    harness.run_ok();
    harness.get_by_label("Run & sign off security").click();
    step_until(&mut harness, "the demo run to sign off", |h| h.query_by_label("Open pull request to merge").is_some());
}

#[test]
fn settings_opens_from_the_popover() {
    let mut harness = popover_harness();
    harness.run_ok();
    let at = harness.get_by_label("Settings").rect().center();
    harness.event(egui::Event::PointerMoved(at));
    harness.step();
    let press = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    harness.event(press(true));
    harness.step();
    harness.event(press(false));
    harness.run_ok();
    assert!(harness.state().settings_open);
}

#[test]
fn the_first_run_screen_picks_a_folder_and_keeps_settings_second() {
    let mut harness = popover_harness();
    // Let the demo's first refresh land, or it would replace the state below.
    step_until(&mut harness, "the first refresh", |h| !h.state().refreshing());
    {
        let app = harness.state_mut();
        app.state = ProductState {
            fetched_at: Some(jiff::Timestamp::now()),
            env: signoff_core::Environment { gh_installed: true, authenticated: true, ..Default::default() },
            onboarded: false,
            ..ProductState::default()
        };
    }
    harness.run_ok();
    harness.get_by_label("Track a repository");
    harness.get_by_label("Choose a folder…");
    harness.get_by_label("Open settings").click();
    harness.run_ok();
    assert!(harness.state().settings_open);
}

#[test]
fn set_up_signoff_opens_branch_rules_ready_to_save() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = Arc::new(
        Script::default()
            .reply("check-ref-format", "main")
            .reply("protection --jq", r#"["ci/build"]"#)
            .reply("--method", "{}"),
    );
    let service = service(dir.path(), vec![tracked(dir.path())], script.clone());
    let state = service.cached_or_config_state();
    let mut harness = settings_harness(service, state);
    harness.run_ok();
    harness.get_by_label("Repositories").click();
    harness.run_ok();

    harness.get_by_label("Set up signoff").click();
    step_until(&mut harness, "the rules to load with signoff ready", |h| {
        h.query_by_label_contains("`signoff` is ready below").is_some()
    });
    harness.get_by_label("ci/build");
    harness.get_by_label("Save to GitHub").click();
    step_until(&mut harness, "the rules to be written", |_| {
        script.calls().iter().any(|(line, _)| line.contains("--method"))
    });

    let calls = script.calls();
    let (_, input) = calls.iter().find(|(line, _)| line.contains("--method")).expect("a write");
    let input = input.as_deref().unwrap_or_default();
    assert!(input.contains(r#""signoff""#) && input.contains("ci/build"), "{input}");
}

#[test]
fn a_pull_request_without_signoffs_offers_to_set_them_up() {
    let mut harness = popover_harness();
    harness.run_ok();
    {
        let app = harness.state_mut();
        let pr = app.state.pull_requests.first_mut().expect("a pull request");
        pr.checks.clear();
        pr.rules.signoff_contexts.clear();
        let (repo_id, number) = (pr.repo_id.clone(), pr.pr.number);
        app.glance.page = popover::Page::PullRequest { repo_id, number };
    }
    harness.run_ok();
    harness.get_by_label("No signoffs required");
    harness.get_by_label("Set up signoff").click();
    harness.run_ok();
    assert!(harness.state().settings_open);
    assert_eq!(harness.state().settings.section, settings::Section::BranchRules);
}
