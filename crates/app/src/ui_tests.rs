//! UI tests: the real popover and settings, driven through their
//! accessibility tree with egui_kittest. No window, GPU or GitHub is needed;
//! `gh` and `git` are answered by [`Script`].

use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use signoff_core::config::{AppConfig, CommandMapping, ConfigStore, TrackedRepository};
use signoff_core::{Gh, Health, HistoryStore, Output, ProductService, ProductState, Runner};

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
    /// How many times Settings asked to install `gh signoff`.
    installs: usize,
    /// Read the state again on refresh, as the app does. Off, the state
    /// stays as it was: a refresh that has not finished yet.
    follow: bool,
}

fn settings_harness(service: Arc<ProductService>, state: ProductState) -> Harness<'static, SettingsUnderTest> {
    let mut settings = Settings::new(service.clone(), || {});
    // Never scan the machine running the tests.
    settings.find_checkouts_in(None);
    let under_test = SettingsUnderTest { settings, state, service, refreshes: 0, installs: 0, follow: true };
    Harness::builder().with_size(egui::vec2(820.0, 900.0)).build_ui_state(
        |ui, t: &mut SettingsUnderTest| {
            if !installed(ui.ctx()) {
                return;
            }
            let effects = settings::contents(ui, &mut t.settings, &t.state);
            t.installs += usize::from(effects.install);
            if effects.refresh {
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
    assert!(harness.query_by_label("Add a repository").is_none());
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
    step_until(&mut harness, "the checkout to be added", |h| {
        h.query_by_label("Add a repository").is_some()
            && h.state().service.config().is_some_and(|c| !c.snapshot().repositories.is_empty())
    });

    let repos = service.config().expect("config").snapshot().repositories;
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0].name_with_owner, "acme/widgets");
    assert!(harness.query_by_label("acme/widgets").is_some());
}

#[test]
fn the_watching_switch_flips_before_the_refresh_finishes() {
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
    assert!(harness.query_by_label_contains("Not watching").is_some(), "the switch should flip at once");
    assert!(!service.config().expect("config").find_repository("repo1").expect("repo").enabled);

    harness.get_by_label("Stop tracking").click();
    harness.run_ok();
    harness.get_by_label("Remove").click();
    harness.run_ok();
    assert!(harness.query_by_label("acme/widgets").is_none(), "the row should go at once");
    assert!(service.config().expect("config").snapshot().repositories.is_empty());
}

#[test]
fn the_row_reports_pull_requests_and_failures() {
    let dir = tempfile::tempdir().expect("tempdir");
    let service = service(dir.path(), vec![tracked(dir.path())], Arc::new(Script::default()));
    let mut state = service.cached_or_config_state();
    state.repositories[0].pull_request_count = 3;
    let pr = |status| signoff_core::PullRequestState {
        repo_id: "repo1".into(),
        status,
        ..signoff_core::PullRequestState::default()
    };
    state.pull_requests = vec![pr(Health::Failing), pr(Health::Ready), pr(Health::Attention)];
    let mut harness = settings_harness(service, state);
    harness.run_ok();
    harness.get_by_label("Repositories").click();
    harness.run_ok();
    harness.get_by_label("3 open PRs · 1 failing");
    for action in ["Open on GitHub", "Set up signoff", "Stop tracking"] {
        harness.get_by_label(action);
    }
    assert!(harness.query_by_label_contains("Show in ").is_some());
}

#[test]
fn the_row_shows_why_it_could_not_be_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let service = service(dir.path(), vec![tracked(dir.path())], Arc::new(Script::default()));
    let mut state = service.cached_or_config_state();
    state.repositories[0].error = Some("gh: not logged in".into());
    let mut harness = settings_harness(service, state);
    harness.run_ok();
    harness.get_by_label("Repositories").click();
    harness.run_ok();
    harness.get_by_label_contains("gh auth login");
}

#[test]
fn keeping_a_repository_cancels_the_removal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let service = service(dir.path(), vec![tracked(dir.path())], Arc::new(Script::default()));
    let state = service.cached_or_config_state();
    let mut harness = settings_harness(service.clone(), state);
    harness.run_ok();
    harness.get_by_label("Repositories").click();
    harness.run_ok();
    harness.get_by_label("Stop tracking").click();
    harness.run_ok();
    assert!(harness.query_by_label("Stop tracking").is_none(), "asks first");
    harness.get_by_label("Keep").click();
    harness.run_ok();
    harness.get_by_label("Stop tracking");
    assert_eq!(service.config().expect("config").snapshot().repositories.len(), 1);
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
    let checkouts = crate::checkouts::Checkouts::new(Gh::default(), true, || {});
    let installer = crate::installer::Installer::new(Gh::default(), true, || {});
    let mut app = Signoff::new(None, refresher, runs, checkouts, installer, settings, true);
    app.state = service.refresh();
    app
}

fn popover_harness() -> Harness<'static, Signoff> {
    Harness::builder().with_size(egui::vec2(400.0, 660.0)).build_ui_state(
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

#[test]
fn a_signed_off_check_can_be_run_again() {
    let mut harness = popover_harness();
    harness.run_ok();
    harness.get_by_label_contains("Add label-based partial signoff").click();
    harness.run_ok();
    // Tapping a signed-off row picks it, and the main button offers a rerun.
    harness.get_by_label_contains("lint").click();
    harness.run_ok();
    assert!(harness.query_by_label("Run lint again").is_some());
}

/// The page fits a 400x660 window: no control or line is cut off.
#[test]
fn the_pull_request_page_fits_the_window() {
    let size = egui::vec2(400.0, 660.0);
    let mut harness = Harness::builder().with_size(size).build_ui_state(
        |ui, app: &mut Signoff| {
            if installed(ui.ctx()) {
                app.tick(Some(ui.ctx()));
                popover::show(ui, app);
            }
        },
        demo_app(),
    );
    harness.run_ok();
    harness.get_by_label_contains("Add label-based partial signoff").click();
    harness.run_ok();
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
    let button = harness.get_by_label("Run & sign off security");
    let rect = button.rect();
    assert!(screen.contains_rect(rect), "button at {rect:?} is outside {screen:?}");
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

/// A folder shaped like a GitHub checkout under `home/code`.
fn github_checkout(home: &Path, name: &str) -> std::path::PathBuf {
    let dir = home.join("code").join(name);
    std::fs::create_dir_all(dir.join(".git")).expect("dot dir");
    std::fs::write(dir.join(".git/config"), format!("[remote \"origin\"]\n\turl = git@github.com:acme/{name}.git\n"))
        .expect("config");
    std::fs::canonicalize(dir).expect("canonical")
}

fn repo_view_script() -> Arc<Script> {
    Arc::new(
        Script::default()
            .reply("repo view", r#"{"nameWithOwner":"acme/widgets","defaultBranchRef":{"name":"main"}}"#)
            .reply("rev-parse", "main"),
    )
}

#[test]
fn settings_offers_checkouts_found_on_this_computer_and_adds_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    github_checkout(&home, "widgets");
    github_checkout(&home, "gadgets");
    let service = service(dir.path(), Vec::new(), repo_view_script());
    let mut harness = settings_harness(service.clone(), ProductState::default());
    harness.state_mut().settings.find_checkouts_in(Some(home));
    harness.run_ok();
    harness.get_by_label("Repositories").click();
    step_until(&mut harness, "the scan", |h| h.query_by_label("Found on this computer").is_some());
    harness.get_by_label("acme/widgets");
    harness.get_by_label("acme/gadgets");
    harness.get_by_label_contains("Add all 2");

    // The paste field has an Add of its own, above the suggestions'.
    harness.get_all_by_label("Add").nth(1).expect("the first suggestion's Add button").click();
    step_until(&mut harness, "the checkout to be added", |h| {
        h.state().service.config().map_or(0, |c| c.snapshot().repositories.len()) >= 1
    });
    let repos = service.config().expect("config").snapshot().repositories;
    assert_eq!(repos.len(), 1);
    assert!(repos[0].path.ends_with("gadgets"), "the first suggestion is the one added");
    // The added one is no longer offered; the other still is. (The script
    // names every repository acme/widgets, so it shows as row and suggestion.)
    step_until(&mut harness, "the rescan", |h| h.query_by_label_contains("Add all").is_none());
    assert_eq!(harness.get_all_by_label("acme/widgets").count(), 2);
    assert!(harness.query_by_label("acme/gadgets").is_none());
}

#[test]
fn add_all_tracks_every_suggestion() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    github_checkout(&home, "widgets");
    github_checkout(&home, "gadgets");
    let service = service(dir.path(), Vec::new(), repo_view_script());
    let mut harness = settings_harness(service.clone(), ProductState::default());
    harness.state_mut().settings.find_checkouts_in(Some(home));
    harness.run_ok();
    harness.get_by_label("Repositories").click();
    step_until(&mut harness, "the scan", |h| h.query_by_label_contains("Add all 2").is_some());
    harness.get_by_label_contains("Add all 2").click();
    step_until(&mut harness, "both to be added", |h| {
        h.state().service.config().map_or(0, |c| c.snapshot().repositories.len()) >= 2
    });
    assert_eq!(service.config().expect("config").snapshot().repositories.len(), 2);
    step_until(&mut harness, "the suggestions to go", |h| h.query_by_label("Found on this computer").is_none());
}

#[test]
fn already_tracked_checkouts_are_not_suggested() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    let widgets = github_checkout(&home, "widgets");
    github_checkout(&home, "gadgets");
    let service = service(dir.path(), vec![tracked(&widgets)], repo_view_script());
    let mut harness = settings_harness(service, ProductState::default());
    harness.state_mut().settings.find_checkouts_in(Some(home));
    harness.run_ok();
    harness.get_by_label("Repositories").click();
    step_until(&mut harness, "the scan", |h| h.query_by_label("Found on this computer").is_some());
    harness.get_by_label("acme/gadgets");
    assert!(harness.query_by_label_contains("Add all").is_none());
}

#[derive(Debug)]
struct Dropped(std::path::PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        Err("not needed".into())
    }
}

#[test]
fn a_folder_dropped_on_settings_is_tracked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let checkout = dir.path().join("widgets");
    std::fs::create_dir(&checkout).expect("checkout");
    let service = service(dir.path(), Vec::new(), repo_view_script());
    let mut harness = settings_harness(service.clone(), ProductState::default());
    harness.run_ok();
    harness.get_by_label("Repositories").click();
    harness.run_ok();
    harness.input_mut().dropped_files.push(Arc::new(Dropped(checkout)));
    step_until(&mut harness, "the folder to be added", |h| {
        h.state().service.config().map_or(0, |c| c.snapshot().repositories.len()) >= 1
    });
    assert_eq!(service.config().expect("config").snapshot().repositories.len(), 1);
}

#[test]
fn the_first_run_screen_lists_suggestions_and_adds_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    let widgets = github_checkout(&home, "widgets");
    let service = service(dir.path(), Vec::new(), repo_view_script());
    let mut harness = popover_harness();
    step_until(&mut harness, "the first refresh", |h| !h.state().refreshing());
    {
        let app = harness.state_mut();
        app.settings = Settings::new(service.clone(), || {});
        app.settings
            .suggest(vec![signoff_core::discover::Found { path: widgets, name_with_owner: "acme/widgets".into() }]);
        app.state = ProductState {
            fetched_at: Some(jiff::Timestamp::now()),
            env: signoff_core::Environment { gh_installed: true, authenticated: true, ..Default::default() },
            onboarded: false,
            ..ProductState::default()
        };
    }
    harness.run_ok();
    harness.get_by_label("Found on this computer");
    harness.get_by_label("acme/widgets");
    harness.get_by_label("Choose a folder…");
    harness.get_by_label("Add").click();
    step_until(&mut harness, "the checkout to be added", |_| {
        !service.config().expect("config").snapshot().repositories.is_empty()
    });
    step_until(&mut harness, "the suggestion to go", |h| h.query_by_label("Found on this computer").is_none());
}

/// Opens the first demo pull request with `security` lacking a command.
fn unmapped_security_page() -> Harness<'static, Signoff> {
    let mut harness = popover_harness();
    harness.run_ok();
    harness.get_by_label_contains("Add label-based partial signoff").click();
    harness.run_ok();
    {
        let app = harness.state_mut();
        let pr = app.state.pull_requests.iter_mut().find(|pr| pr.pr.number == 482).expect("pull request");
        let check = pr.checks.iter_mut().find(|check| check.name == "security").expect("check");
        check.configured = false;
        check.local_command.clear();
    }
    harness.run_ok();
    harness
}

#[test]
fn a_check_without_a_command_offers_to_configure_it() {
    let mut harness = unmapped_security_page();
    assert!(harness.query_by_label("Run & sign off security").is_none());
    harness.get_by_label("Configure").click();
    harness.run_ok();
    assert!(harness.state().settings_open);
    assert_eq!(harness.state().settings.section, settings::Section::Commands);
}

#[test]
fn the_pages_button_configures_the_blocking_check() {
    let mut harness = unmapped_security_page();
    harness.get_by_label("Configure security").click();
    harness.run_ok();
    assert!(harness.state().settings_open);
    assert_eq!(harness.state().settings.section, settings::Section::Commands);
}

#[test]
fn configuring_a_check_selects_its_repository_and_offers_commands_from_the_checkout() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("package.json"), r#"{"scripts":{"build":"b","lint":"eslint .","test":"jest"}}"#)
        .expect("package.json");
    std::fs::write(dir.path().join("pnpm-lock.yaml"), "").expect("lockfile");
    let other = tempfile::tempdir().expect("tempdir");
    let mut second = tracked(other.path());
    second.id = "repo2".into();
    second.name_with_owner = "acme/gadgets".into();
    let service = service(dir.path(), vec![second, tracked(dir.path())], Arc::new(Script::default()));
    let state = service.cached_or_config_state();
    let mut harness = settings_harness(service.clone(), state);
    harness.run_ok();
    harness.state_mut().settings.configure_check("repo1", "lint");
    harness.run_ok();

    // The check has its row, with the command field focused.
    harness.get_by_label("lint");
    let field = harness.get_all_by_role(egui::accesskit::Role::TextInput).next().expect("a command field");
    assert!(field.is_focused(), "the command field takes focus");
    // The matching command comes first.
    let chips: Vec<String> = harness
        .get_all_by_role(egui::accesskit::Role::Button)
        .filter_map(|node| node.accesskit_node().label())
        .filter(|label| label.starts_with("pnpm run"))
        .collect();
    assert_eq!(chips, ["pnpm run lint", "pnpm run build", "pnpm run test"]);

    harness.get_by_label("pnpm run lint").click();
    harness.run_ok();
    assert!(harness.query_by_label("pnpm run build").is_none(), "suggestions go once the field is filled");
    harness.get_by_label("Save commands").click();
    harness.run_ok();
    let repo = service.config().expect("config").find_repository("repo1").expect("repo");
    assert_eq!(repo.commands["lint"].command, "pnpm run lint");
}

#[test]
fn commands_set_up_from_ci_show_where_they_came_from_and_follow_the_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let service = service(dir.path(), vec![tracked(dir.path())], Arc::new(Script::default()));
    let state = service.cached_or_config_state();
    let mut harness = settings_harness(service.clone(), state);
    harness.state_mut().settings.section = settings::Section::Commands;
    harness.run_ok();
    assert!(harness.query_by_label("cargo test --workspace").is_none());

    // Detection lands while the page is open, and the untouched page shows it.
    let found = std::collections::BTreeMap::from([(
        "tests".to_owned(),
        signoff_core::DetectedCommand {
            command: "cargo test --workspace".into(),
            source: ".github/workflows/rust.yml".into(),
        },
    )]);
    assert!(service.config().expect("config").adopt_detected("repo1", &found).expect("adopt"));
    harness.run_ok();
    harness.get_by_label("tests");
    harness.get_by_label("Set up from .github/workflows/rust.yml");
    let field = harness.get_all_by_role(egui::accesskit::Role::TextInput).next().expect("a command field");
    assert_eq!(field.value().as_deref(), Some("cargo test --workspace"));
}

/// A popover harness on the demo data where `gh` and `git` answer through
/// `script`, as outside demo mode, opened on the pull request that is not
/// checked out.
fn unchecked_page(script: Arc<Script>) -> Harness<'static, Signoff> {
    let mut app = demo_app();
    app.demo = false;
    app.checkouts = crate::checkouts::Checkouts::new(Gh::new(script), false, || {});
    let (repo_id, number) = {
        let pr = app.state.pull_requests.iter().find(|pr| !pr.checked_out).expect("an unchecked pull request");
        (pr.repo_id.clone(), pr.pr.number)
    };
    app.glance.page = popover::Page::PullRequest { repo_id, number };
    Harness::builder().with_size(egui::vec2(400.0, 660.0)).build_ui_state(
        |ui, app: &mut Signoff| {
            if installed(ui.ctx()) {
                app.tick(Some(ui.ctx()));
                popover::show(ui, app);
            }
        },
        app,
    )
}

#[test]
fn a_pull_request_that_is_not_checked_out_offers_to_check_it_out() {
    let script = Arc::new(Script::default().reply("status --porcelain", "").reply("pr checkout", ""));
    let mut harness = unchecked_page(script.clone());
    harness.run_ok();
    assert!(harness.query_by_label_contains("Run & sign off").is_none());
    harness.get_by_label("Check out #485").click();
    step_until(&mut harness, "gh pr checkout to run", |_| {
        script.calls().iter().any(|(line, _)| line.contains("gh pr checkout 485"))
    });
    let lines: Vec<String> = script.calls().into_iter().map(|(line, _)| line).collect();
    assert_eq!(lines, ["git status --porcelain --untracked-files=no", "gh pr checkout 485"]);
}

#[test]
fn a_dirty_checkout_is_refused_with_a_reason_and_gh_never_runs() {
    let script = Arc::new(Script::default().reply("status --porcelain", " M src/lib.rs").reply("pr checkout", ""));
    let mut harness = unchecked_page(script.clone());
    harness.run_ok();
    harness.get_by_label("Check out #485").click();
    step_until(&mut harness, "the refusal", |h| h.query_by_label_contains("uncommitted change").is_some());
    harness.get_by_label("Check out #485");
    assert!(!script.calls().iter().any(|(line, _)| line.contains("pr checkout")));
}

#[test]
fn the_demo_simulates_a_checkout_and_then_offers_to_run() {
    let mut harness = popover_harness();
    harness.run_ok();
    {
        let app = harness.state_mut();
        let pr = app.state.pull_requests.iter().find(|pr| !pr.checked_out).expect("an unchecked pull request");
        let (repo_id, number) = (pr.repo_id.clone(), pr.pr.number);
        app.glance.page = popover::Page::PullRequest { repo_id, number };
    }
    harness.run_ok();
    harness.get_by_label("Check out #485").click();
    step_until(&mut harness, "the simulated checkout", |h| h.query_by_label_contains("Run & sign off").is_some());
}

#[test]
fn the_notifications_switch_is_on_by_default_and_saved() {
    let dir = tempfile::tempdir().expect("tempdir");
    let service = service(dir.path(), vec![tracked(dir.path())], Arc::new(Script::default()));
    let state = service.cached_or_config_state();
    let mut harness = settings_harness(service.clone(), state);
    harness.run_ok();
    assert!(harness.state().settings.notifications_on());

    harness.get_by_label("Notifications").click();
    harness.run_ok();
    assert!(!service.config().expect("config").snapshot().notifications);
    assert!(!harness.state().settings.notifications_on());

    harness.get_by_label("Notifications").click();
    harness.run_ok();
    assert!(service.config().expect("config").snapshot().notifications);
}

#[test]
fn the_global_shortcut_switch_is_on_by_default_and_saved() {
    let dir = tempfile::tempdir().expect("tempdir");
    let service = service(dir.path(), vec![tracked(dir.path())], Arc::new(Script::default()));
    let state = service.cached_or_config_state();
    let mut harness = settings_harness(service.clone(), state);
    harness.run_ok();
    assert_eq!(harness.state().settings.shortcut_config(), (true, String::new()));

    harness.get_by_label("Global shortcut").click();
    harness.run_ok();
    assert!(!service.config().expect("config").snapshot().shortcut_enabled);

    harness.get_by_label("Global shortcut").click();
    harness.run_ok();
    assert!(service.config().expect("config").snapshot().shortcut_enabled);
}

#[test]
fn the_separate_worktree_switch_is_on_by_default_and_saved() {
    let dir = tempfile::tempdir().expect("tempdir");
    let service = service(dir.path(), vec![tracked(dir.path())], Arc::new(Script::default()));
    let state = service.cached_or_config_state();
    let mut harness = settings_harness(service.clone(), state);
    harness.run_ok();
    assert!(harness.state().settings.isolated_worktrees_on());

    harness.get_by_label("Verify in a separate worktree").click();
    harness.run_ok();
    assert!(!service.config().expect("config").snapshot().isolated_worktrees);

    harness.get_by_label("Verify in a separate worktree").click();
    harness.run_ok();
    assert!(service.config().expect("config").snapshot().isolated_worktrees);
}

/// Answers like [`Script`], but holds every `git fetch` until opened and
/// then fails it, as an unreachable remote does.
struct HeldFetch {
    script: Script,
    open: std::sync::atomic::AtomicBool,
}

impl Runner for HeldFetch {
    fn run(&self, dir: Option<&Path>, input: Option<&str>, program: &str, args: &[&str]) -> io::Result<Output> {
        if program == "git" && args.first() == Some(&"fetch") {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.open.load(std::sync::atomic::Ordering::Acquire) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            return Ok(Output {
                stdout: String::new(),
                stderr: "fatal: unable to access 'https://github.com/basecamp/gh-signoff.git/': Could not resolve host"
                    .into(),
                code: 128,
            });
        }
        self.script.run(dir, input, program, args)
    }
}

/// The demo's pull request #485, which is not checked out, outside demo
/// mode with the separate-worktree setting `on`: runs go through `runner`
/// into worktrees under a temporary folder.
fn worktree_page(dir: &Path, on: bool, runner: Arc<dyn Runner>) -> Harness<'static, Signoff> {
    let checkout = dir.join("gh-signoff");
    std::fs::create_dir_all(checkout.join(".git")).expect("mkdir");
    std::fs::write(checkout.join(".git").join("HEAD"), "0123456789abcdef0123456789abcdef01234567\n").expect("HEAD");
    let repo = TrackedRepository {
        id: "demo-signoff".into(),
        path: checkout,
        name_with_owner: "basecamp/gh-signoff".into(),
        enabled: true,
        commands: ["tests", "lint"]
            .into_iter()
            .map(|name| (name.to_owned(), CommandMapping { command: "bin/test".into(), timeout_seconds: 60 }))
            .collect(),
        ..TrackedRepository::default()
    };
    let config = Arc::new(ConfigStore::with_config(
        dir.join("config.json"),
        AppConfig { isolated_worktrees: on, repositories: vec![repo], ..AppConfig::default() },
    ));
    let history = Arc::new(HistoryStore::open(dir.join("history.json")).expect("history"));
    let settings_service =
        Arc::new(ProductService::new(Gh::new(Arc::new(Script::default())), config.clone(), history.clone(), None));

    let mut app = demo_app();
    app.demo = false;
    app.settings = Settings::new(settings_service, || {});
    let demo = Arc::new(ProductService::demo());
    demo.refresh();
    let verifier = signoff_core::Verifier::new(demo, runner, config, history)
        .with_worktrees(signoff_core::Worktrees::new(dir.join("worktrees")));
    app.runs = crate::runs::Runs::new(Arc::new(verifier), || {});
    app.glance.page = popover::Page::PullRequest { repo_id: "demo-signoff".into(), number: 485 };
    Harness::builder().with_size(egui::vec2(400.0, 660.0)).build_ui_state(
        |ui, app: &mut Signoff| {
            if installed(ui.ctx()) {
                app.tick(Some(ui.ctx()));
                popover::show(ui, app);
            }
        },
        app,
    )
}

/// What a checkout on another commit answers, and the git steps before
/// the fetch.
fn elsewhere() -> Script {
    Script::default()
        .reply("rev-parse HEAD", "0123456789abcdef0123456789abcdef01234567")
        .reply("worktree prune", "")
        .reply("worktree add", "")
        .reply("remote -v", "origin\thttps://github.com/basecamp/gh-signoff.git (fetch)")
}

#[test]
fn with_the_setting_off_a_checkout_elsewhere_still_offers_to_check_it_out() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut harness = worktree_page(dir.path(), false, Arc::new(elsewhere()));
    harness.run_ok();
    harness.get_by_label("Check out #485");
    assert!(harness.query_by_label_contains("Run & sign off").is_none());
    assert!(harness.query_by_label_contains("separate worktree").is_none());
}

#[test]
fn with_the_setting_on_a_checkout_elsewhere_runs_in_a_worktree_and_explains_a_failed_fetch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = Arc::new(HeldFetch { script: elsewhere(), open: std::sync::atomic::AtomicBool::new(false) });
    let mut harness = worktree_page(dir.path(), true, runner.clone());
    harness.run_ok();
    assert!(harness.query_by_label_contains("your checkout is not touched").is_some());
    harness.get_by_label("Check out #485");

    harness.get_by_label_contains("Run & sign off").click();
    step_until(&mut harness, "the worktree to be prepared", |h| {
        h.query_by_label_contains("Preparing worktree…").is_some()
    });
    assert!(harness.query_by_label_contains("Stop").is_some(), "a preparation can be stopped");
    assert!(harness.query_by_label("Check out #485").is_none(), "no checkout while a run goes");

    runner.open.store(true, std::sync::atomic::Ordering::Release);
    step_until(&mut harness, "the failed fetch", |h| {
        h.query_by_label_contains("could not fetch #485 from origin").is_some()
    });
    assert!(harness.query_by_label_contains("git fetch origin").is_some(), "the fix is named");
    assert!(harness.query_by_label_contains("Preparing worktree…").is_none());
    let calls: Vec<String> = runner.script.calls().into_iter().map(|(line, _)| line).collect();
    assert!(!calls.iter().any(|line| line.contains("bin/test") || line.contains("signoff --commit")), "{calls:?}");
}

/// With the setting on, "Check out #N" sits under "Run & sign off" and
/// asks before switching the checkout's branch; cancelling runs nothing.
#[test]
fn with_the_setting_on_checking_out_asks_before_switching_branches() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = Arc::new(elsewhere().reply("status --porcelain", "").reply("pr checkout", ""));
    let mut harness = worktree_page(dir.path(), true, script.clone());
    harness.state_mut().checkouts = crate::checkouts::Checkouts::new(Gh::new(script.clone()), false, || {});
    harness.run_ok();

    harness.get_by_label("Check out #485").click();
    harness.run_ok();
    assert!(harness.query_by_label_contains("This switches gh-signoff").is_some(), "the switch is named");
    harness.get_by_label("Cancel").click();
    harness.run_ok();
    assert!(harness.query_by_label("Switch branch").is_none());
    assert!(harness.query_by_label_contains("your checkout is not touched").is_some());

    harness.get_by_label("Check out #485").click();
    harness.run_ok();
    harness.get_by_label("Switch branch").click();
    step_until(&mut harness, "gh pr checkout to run", |_| {
        script.calls().iter().any(|(line, _)| line.contains("gh pr checkout 485"))
    });
    harness.get_by_label_contains("Run & sign off");
}

/// A state where `gh` is ready and only the extension is missing.
fn without_extension(mut state: ProductState) -> ProductState {
    state.env.gh_installed = true;
    state.env.authenticated = true;
    state.env.login = "me".into();
    state.env.signoff_installed = false;
    state
}

#[test]
fn a_missing_extension_offers_to_install_it_from_the_popover() {
    let script = Arc::new(Script::default().reply("extension install", ""));
    let mut app = demo_app();
    app.demo = false;
    app.installer = crate::installer::Installer::new(Gh::new(script.clone()), false, || {});
    let mut harness = Harness::builder().with_size(egui::vec2(400.0, 660.0)).build_ui_state(
        |ui, app: &mut Signoff| {
            if installed(ui.ctx()) {
                app.tick(Some(ui.ctx()));
                popover::show(ui, app);
            }
        },
        app,
    );
    harness.run_ok();
    // The first frame took in the refresher's state; now it lacks the extension.
    let state = without_extension(harness.state().state.clone());
    harness.state_mut().state = state;
    harness.run_ok();
    harness.get_by_label_contains("gh signoff is not installed");
    harness.get_by_label("Install gh signoff").click();
    step_until(&mut harness, "gh extension install to run", |_| {
        script.calls().iter().any(|(line, _)| line == "gh extension install basecamp/gh-signoff")
    });
    step_until(&mut harness, "the install to finish", |h| !h.state().installer.running());
}

#[test]
fn a_failed_install_says_why_and_offers_to_try_again() {
    let script = Arc::new(Script::default());
    let mut app = demo_app();
    app.demo = false;
    app.installer = crate::installer::Installer::new(Gh::new(script), false, || {});
    let mut harness = Harness::builder().with_size(egui::vec2(400.0, 660.0)).build_ui_state(
        |ui, app: &mut Signoff| {
            if installed(ui.ctx()) {
                app.tick(Some(ui.ctx()));
                popover::show(ui, app);
            }
        },
        app,
    );
    harness.run_ok();
    let state = without_extension(harness.state().state.clone());
    harness.state_mut().state = state;
    harness.run_ok();
    harness.get_by_label("Install gh signoff").click();
    step_until(&mut harness, "the failure", |h| h.query_by_label_contains("could not install gh signoff").is_some());
    harness.get_by_label("Install gh signoff");
}

#[test]
fn an_installed_extension_offers_nothing() {
    let mut harness = popover_harness();
    harness.run_ok();
    assert!(harness.query_by_label("Install gh signoff").is_none());
}

#[test]
fn settings_offers_install_only_when_the_extension_is_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let service = service(dir.path(), vec![tracked(dir.path())], Arc::new(Script::default()));
    let missing = without_extension(service.cached_or_config_state());
    let mut harness = settings_harness(service.clone(), missing);
    harness.run_ok();
    harness.get_by_label("Install").click();
    harness.run_ok();
    assert_eq!(harness.state().installs, 1);

    let mut ready = service.cached_or_config_state();
    ready.env.gh_installed = true;
    ready.env.authenticated = true;
    ready.env.signoff_installed = true;
    let mut harness = settings_harness(service, ready);
    harness.run_ok();
    assert!(harness.query_by_label("Install").is_none());
}

fn palette_open(harness: &Harness<'static, Signoff>) -> bool {
    harness.state().glance.palette.is_some()
}

#[test]
fn the_command_shortcut_opens_and_closes_the_palette() {
    let mut harness = popover_harness();
    harness.run_ok();
    assert!(!palette_open(&harness));
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    harness.run_ok();
    assert!(palette_open(&harness));
    assert!(harness.query_by_label_contains("Open #").is_some());
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    harness.run_ok();
    assert!(!palette_open(&harness));
}

#[test]
fn the_band_button_opens_the_palette() {
    let mut harness = popover_harness();
    harness.run_ok();
    harness.get_by_label_contains("Search and run actions").click();
    harness.run_ok();
    assert!(palette_open(&harness));
}

#[test]
fn typing_filters_the_palette_and_enter_opens_the_pull_request() {
    let mut harness = popover_harness();
    harness.run_ok();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    harness.run_ok();
    harness.event(egui::Event::Text("poller".into()));
    harness.run_ok();
    assert!(harness.query_by_label_contains("Refresh all repositories").is_none(), "filtered out");
    assert!(harness.query_by_label_contains("#485 Fix race condition in poller").is_some());
    harness.key_press(egui::Key::Enter);
    harness.run_ok();
    assert!(!palette_open(&harness));
    let page = harness.state().glance.page.clone();
    let popover::Page::PullRequest { repo_id, number } = page else { panic!("a pull request page, got {page:?}") };
    let pr = harness.state().state.pull_request(&repo_id, number).expect("pr");
    assert!(pr.pr.title.contains("poller"));
}

#[test]
fn arrow_keys_move_the_selection_before_enter() {
    let mut harness = popover_harness();
    harness.run_ok();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    harness.run_ok();
    harness.key_press(egui::Key::ArrowDown);
    harness.run_ok();
    harness.key_press(egui::Key::ArrowUp);
    harness.run_ok();
    harness.key_press(egui::Key::ArrowDown);
    harness.run_ok();
    assert_eq!(harness.state().glance.palette.as_ref().map(|p| p.selected()), Some(1));
}

#[test]
fn clicking_a_result_runs_it() {
    let mut harness = popover_harness();
    harness.run_ok();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    harness.run_ok();
    harness.event(egui::Event::Text("manage".into()));
    harness.run_ok();
    harness.get_by_label_contains("Manage repositories").click();
    harness.run_ok();
    assert!(!palette_open(&harness));
    assert!(harness.state().settings_open);
    assert_eq!(harness.state().settings.section, settings::Section::Repositories);
}

#[test]
fn escape_closes_the_palette_and_is_taken_from_the_window() {
    let mut harness = popover_harness();
    harness.run_ok();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    harness.run_ok();
    harness.key_press(egui::Key::Escape);
    harness.run_ok();
    assert!(!palette_open(&harness));
    // The window's own handler hides the popover on Escape only while the
    // palette is closed; it is open when that handler runs for this key.
}

/// The foot of the list says whether what it shows is current, in each of
/// the scheduler's states.
#[test]
fn the_live_indicator_says_how_reads_are_going() {
    use crate::glance::Liveness;
    let until = jiff::Timestamp::now() + jiff::SignedDuration::from_mins(30);
    let cases = [
        (Liveness::Live, "Live"),
        (Liveness::Reading, "Reading…"),
        (Liveness::Offline { retry_in: Some(Duration::from_secs(20)) }, "Offline · retrying in 20s"),
        (Liveness::Offline { retry_in: None }, "Offline · retrying"),
        (Liveness::RateLimited { until }, "Rate limited until "),
    ];
    for (live, label) in cases {
        let mut harness = Harness::builder().with_size(egui::vec2(380.0, 60.0)).build_ui(move |ui| {
            if installed(ui.ctx()) {
                popover::live_line(ui, live, false, None);
            }
        });
        harness.run_ok();
        assert!(harness.query_by_label_contains(label).is_some(), "{live:?} should read {label}");
    }
}

/// The demo app goes live once its first read settles, and the old
/// "Updated 12s ago" line is gone.
#[test]
fn the_popover_goes_live_after_the_first_read() {
    let mut harness = popover_harness();
    step_until(&mut harness, "the first read to settle", |h| h.query_by_label("Live").is_some());
    assert!(harness.query_by_label_contains("Updated").is_none());
}

/// General explains how updates work instead of offering an interval.
#[test]
fn general_has_no_refresh_interval_to_set() {
    let (service, state) = demo_state();
    let mut harness = settings_harness(service, state);
    harness.run_ok();
    assert!(harness.query_by_label_contains("Refresh every").is_none());
    assert!(harness.query_by_role(egui::accesskit::Role::Slider).is_none());
    assert!(harness.query_by_label_contains("every few seconds while the popover is open").is_some());
}
