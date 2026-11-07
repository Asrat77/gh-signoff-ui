//! Repositories: the local checkouts Signoff tracks, whose pull requests
//! each one shows, and adding or removing one.

use std::path::PathBuf;

use egui::{Align, Layout, RichText, Ui};
use signoff_core::config::TrackedRepository;
use signoff_core::{Health, ProductState, RepositoryUpdate};
use signoff_ui::theme::space;
use signoff_ui::widgets::{AddButton, Button, ButtonKind, Switch, spinner, toast};
use signoff_ui::{Status, Type, icons, palette};

use super::{Effects, Settings, discovery, parts};

/// What the repositories section keeps between frames.
#[derive(Debug, Default)]
pub(crate) struct Form {
    /// The path typed into the add field.
    path: String,
    /// The checkout being added (the first, when several are).
    adding: Option<PathBuf>,
    /// How many more are queued behind it.
    adding_more: usize,
    /// The repository whose remove button was pressed once.
    confirm_remove: Option<String>,
}

pub(super) fn show(ui: &mut Ui, settings: &mut Settings, state: &ProductState, _: &mut Effects) {
    parts::heading(
        ui,
        "Repositories",
        "Local checkouts of GitHub repositories. Signoff shows their open pull requests and runs checks in them.",
    );
    accept_drops(ui.ctx(), settings);
    let rows = rows(settings, state);
    if rows.is_empty() {
        parts::notice(ui, Status::Neutral, "No repository yet. Add a local checkout to start.");
    }
    for repo in &rows {
        row(ui, settings, repo);
    }
    ui.add_space(space::S);
    discovery::show(ui, settings, false);
    add(ui, settings);
}

impl Form {
    /// Whether a checkout is being added.
    pub(super) const fn is_adding(&self) -> bool {
        self.adding.is_some()
    }
}

/// One repository as the section draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    id: String,
    path: PathBuf,
    name_with_owner: String,
    default_branch: String,
    enabled: bool,
    /// Open pull requests the last refresh found.
    pull_requests: usize,
    /// Of those, the failing ones.
    failing: usize,
    /// It has been read since it was added or resumed.
    read: bool,
    /// Why the last refresh could not read it.
    error: Option<String>,
}

/// The rows to draw. The saved config decides which repositories there are
/// and how they are set, so a click or a removal shows at once; the last
/// refresh only adds what it learned (the error). Demo data has no config,
/// so its rows come from the state.
fn rows(settings: &Settings, state: &ProductState) -> Vec<Row> {
    match settings.service.config() {
        Some(config) => merge_rows(&config.snapshot().repositories, state),
        None => state
            .repositories
            .iter()
            .map(|repo| Row {
                id: repo.id.clone(),
                path: repo.path.clone(),
                name_with_owner: repo.name_with_owner.clone(),
                default_branch: repo.default_branch.clone(),
                enabled: repo.enabled,
                pull_requests: repo.pull_request_count,
                failing: failing(state, &repo.id),
                read: true,
                error: repo.error.clone(),
            })
            .collect(),
    }
}

/// How many of the repository's pull requests are failing.
fn failing(state: &ProductState, id: &str) -> usize {
    state.pull_requests.iter().filter(|pr| pr.repo_id == id && pr.status == Health::Failing).count()
}

/// The line under a row's path: what the last refresh found, or why not.
fn status_line(repo: &Row) -> (String, Status) {
    if !repo.enabled {
        return ("Paused".into(), Status::Neutral);
    }
    if let Some(error) = &repo.error {
        return (error.clone(), Status::Failing);
    }
    if !repo.read {
        return ("Reading…".into(), Status::Neutral);
    }
    let noun = if repo.pull_requests == 1 { "open PR" } else { "open PRs" };
    let mut line = match repo.pull_requests {
        0 => "No open PRs".to_owned(),
        n => format!("{n} {noun}"),
    };
    if repo.failing > 0 {
        line.push_str(&format!(" · {} failing", repo.failing));
        return (line, Status::Failing);
    }
    (line, Status::Neutral)
}

/// The name of the OS file manager, for the button that opens a folder in it.
const fn file_manager() -> &'static str {
    if cfg!(target_os = "macos") {
        "Finder"
    } else if cfg!(target_os = "windows") {
        "Explorer"
    } else {
        "Files"
    }
}

fn merge_rows(tracked: &[TrackedRepository], state: &ProductState) -> Vec<Row> {
    tracked
        .iter()
        .map(|repo| {
            let read = state.repositories.iter().find(|r| r.id == repo.id);
            Row {
                id: repo.id.clone(),
                path: repo.path.clone(),
                name_with_owner: repo.name_with_owner.clone(),
                default_branch: repo.default_branch.clone(),
                enabled: repo.enabled,
                pull_requests: read.map_or(0, |r| r.pull_request_count),
                failing: failing(state, &repo.id),
                read: read.is_some_and(|r| !r.loading),
                error: read.and_then(|r| r.error.clone()),
            }
        })
        .collect()
}

fn row(ui: &mut Ui, settings: &mut Settings, repo: &Row) {
    let read_only = settings.read_only();
    parts::card(ui, |ui| {
        let p = palette(ui.ctx());
        let mut watching = repo.enabled;
        ui.horizontal(|ui| {
            ui.label(RichText::new(icons::REPOSITORY).font(signoff_ui::fonts::icon(18.0)).color(p.secondary));
            ui.vertical(|ui| {
                ui.label(RichText::new(&repo.name_with_owner).font(Type::Row.font()).color(p.text));
                ui.label(RichText::new(repo.path.display().to_string()).font(Type::Mono.font()).color(p.secondary));
                let (line, status) = status_line(repo);
                let color = if status == Status::Neutral { p.secondary } else { status.color(&p) };
                ui.add(egui::Label::new(RichText::new(line).font(Type::Caption.font()).color(color)).wrap());
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let switch = ui.add_enabled(!read_only, Switch::new(&mut watching, "Watching"));
                switch.on_hover_text("Refresh this repository's pull requests, or pause it");
            });
        });
        ui.add_space(space::S);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(Button::new("Open on GitHub").kind(ButtonKind::Secondary).compact().width(130.0))
                .on_hover_text("Open this repository in your browser")
                .clicked()
            {
                crate::browser::open(ui.ctx(), &format!("https://github.com/{}", repo.name_with_owner));
            }
            let manager = format!("Show in {}", file_manager());
            if ui
                .add(Button::new(&manager).kind(ButtonKind::Secondary).compact().width(130.0))
                .on_hover_text("Open the checkout's folder")
                .clicked()
            {
                show_folder(ui, &repo.path);
            }
            ui.add_enabled_ui(!read_only, |ui| {
                if ui
                    .add(Button::new("Set up signoff").kind(ButtonKind::Secondary).compact().width(130.0))
                    .on_hover_text("Require the signoff check on this repository's default branch")
                    .clicked()
                {
                    settings.set_up_signoff(&repo.id, &repo.default_branch);
                }
                if settings.repositories.confirm_remove.as_deref() == Some(repo.id.as_str()) {
                    if ui.add(Button::new("Remove").kind(ButtonKind::Accent).compact().width(84.0)).clicked() {
                        settings.repositories.confirm_remove = None;
                        remove(ui, settings, repo);
                    }
                    if ui.add(Button::new("Keep").kind(ButtonKind::Secondary).compact().width(72.0)).clicked() {
                        settings.repositories.confirm_remove = None;
                    }
                } else if ui
                    .add(Button::new("Stop tracking").kind(ButtonKind::Secondary).compact().width(130.0))
                    .on_hover_text("Stop showing this repository's pull requests")
                    .clicked()
                {
                    settings.repositories.confirm_remove = Some(repo.id.clone());
                }
            });
        });
        if watching != repo.enabled {
            // The config subscription patches the state and reads it again.
            update_repository(ui, settings, &repo.id, |update| update.enabled = watching);
        }
    });
}

/// Opens `path` in the system file manager.
fn show_folder(ui: &Ui, path: &std::path::Path) {
    if !path.is_dir() {
        toast(ui.ctx(), format!("{} is not there any more", path.display()), Status::Failing);
    } else if let Err(err) = open::that_detached(path) {
        toast(ui.ctx(), format!("Could not open {}: {err}", path.display()), Status::Failing);
    }
}

/// Saves a change to the editable settings of the repository `id`.
pub(super) fn update_repository(ui: &Ui, settings: &Settings, id: &str, change: impl FnOnce(&mut RepositoryUpdate)) {
    let Some(config) = settings.service.config() else { return };
    let Some(current) = config.find_repository(id) else {
        toast(ui.ctx(), "That repository is no longer tracked", Status::Failing);
        return;
    };
    let mut update = RepositoryUpdate { enabled: current.enabled, commands: current.commands };
    change(&mut update);
    if let Err(err) = config.update_repository(id, update) {
        toast(ui.ctx(), err.to_string(), Status::Failing);
    }
}

fn remove(ui: &Ui, settings: &mut Settings, repo: &Row) {
    let Some(config) = settings.service.config() else { return };
    match config.remove_repository(&repo.id) {
        Ok(()) => {
            settings.discovery.mark_stale();
            toast(ui.ctx(), format!("Stopped tracking {}", repo.name_with_owner), Status::Neutral);
        }
        Err(err) => toast(ui.ctx(), err.to_string(), Status::Failing),
    }
}

fn add(ui: &mut Ui, settings: &mut Settings) {
    if settings.read_only() {
        return;
    }
    if let Some(path) = &settings.repositories.adding {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(18.0), egui::Sense::hover());
            spinner(ui, rect.center(), 7.0, palette(ui.ctx()).secondary);
            let more = match settings.repositories.adding_more {
                0 => String::new(),
                n => format!(" and {n} more"),
            };
            parts::caption(ui, &format!("Checking {}{more} with gh…", path.display()));
        });
        return;
    }
    if ui.add(AddButton::new("Add a local checkout")).clicked() {
        choose_and_add(ui.ctx(), settings);
        return;
    }
    ui.add_space(space::S);
    ui.horizontal(|ui| {
        let width = ui.available_width() - 96.0;
        let field = parts::input(ui, &mut settings.repositories.path, "…or paste a checkout's path", Type::Mono, width);
        let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let path = settings.repositories.path.trim().to_owned();
        let clicked = ui
            .add(Button::new("Add").kind(ButtonKind::Accent).compact().width(84.0).enabled(!path.is_empty()))
            .clicked();
        if (clicked || entered) && !path.is_empty() {
            start_adding(settings, vec![expand_home(&path)]);
        }
    });
}

/// Asks for a checkout with the system folder picker and starts tracking
/// it in the background. Progress and errors come as toasts. Settings and
/// the popover's first-run screen both add repositories through this.
pub(super) fn choose_and_add(ctx: &egui::Context, settings: &mut Settings) {
    if settings.repositories.adding.is_some() {
        return;
    }
    if let Some(folder) = rfd::FileDialog::new().set_title("Choose a local checkout").pick_folder() {
        toast(ctx, format!("Checking {} with gh…", folder.display()), Status::Neutral);
        start_adding(settings, vec![folder]);
    }
}

/// The checkout being added, while `gh` checks it.
pub(super) fn adding(settings: &Settings) -> Option<&std::path::Path> {
    settings.repositories.adding.as_deref()
}

/// `~/code/app` → the home folder's `code/app`.
fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")), directories::BaseDirs::new()) {
        (Some(rest), Some(base)) => base.home_dir().join(rest),
        _ => PathBuf::from(path),
    }
}

/// Starts tracking every folder in `paths`, one after another, in the
/// background.
pub(super) fn start_adding(settings: &mut Settings, paths: Vec<PathBuf>) {
    let Some(first) = paths.first().cloned() else { return };
    if settings.repositories.adding.is_some() {
        return;
    }
    settings.repositories.adding = Some(first);
    settings.repositories.adding_more = paths.len() - 1;
    settings.jobs.start(move |service| {
        let results: Vec<_> = paths.into_iter().map(|path| (service.add_repository(&path), path)).collect();
        Box::new(move |settings: &mut Settings, ctx: &egui::Context, _: &mut Effects| {
            settings.repositories.adding = None;
            settings.repositories.adding_more = 0;
            let mut tracked = Vec::new();
            for (result, path) in results {
                match result {
                    Ok(repo) => {
                        settings.discovery.added(&path);
                        tracked.push(repo.name_with_owner);
                    }
                    Err(message) => toast(ctx, message, Status::Failing),
                }
            }
            if !tracked.is_empty() {
                settings.repositories.path.clear();
                let what =
                    if tracked.len() == 1 { tracked.remove(0) } else { format!("{} repositories", tracked.len()) };
                toast(ctx, format!("Tracking {what}"), Status::Ready);
            }
        })
    });
}

/// Starts tracking the folders dropped onto the window, if any. A dropped
/// file stands for the folder it is in.
pub(super) fn accept_drops(ctx: &egui::Context, settings: &mut Settings) {
    if settings.read_only() {
        return;
    }
    let mut dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_owned()).collect());
    if dropped.is_empty() {
        return;
    }
    if settings.repositories.adding.is_some() {
        toast(ctx, "Still adding a checkout. Drop it again in a moment.", Status::Neutral);
        return;
    }
    for path in &mut dropped {
        if path.is_file() {
            path.pop();
        }
    }
    dropped.dedup();
    toast(ctx, "Checking the dropped folder with gh…", Status::Neutral);
    start_adding(settings, dropped);
}

#[cfg(test)]
mod tests {
    use super::*;
    use signoff_core::RepositoryState;

    fn tracked(id: &str, enabled: bool) -> TrackedRepository {
        TrackedRepository {
            id: id.into(),
            name_with_owner: format!("acme/{id}"),
            enabled,
            ..TrackedRepository::default()
        }
    }

    #[test]
    fn rows_follow_the_config_and_borrow_errors_from_the_last_read() {
        let state = ProductState {
            repositories: vec![
                // Stale: the config says paused now.
                RepositoryState {
                    id: "a".into(),
                    enabled: true,
                    error: Some("gh failed".into()),
                    ..RepositoryState::default()
                },
                // Removed since: must not appear.
                RepositoryState { id: "gone".into(), ..RepositoryState::default() },
                // Resumed and not read again yet.
                RepositoryState { id: "c".into(), enabled: true, loading: true, ..RepositoryState::default() },
            ],
            ..ProductState::default()
        };
        let rows = merge_rows(&[tracked("a", false), tracked("b", true), tracked("c", true)], &state);
        assert_eq!(rows.len(), 3);
        assert!(!rows[0].enabled);
        assert!(rows[0].read);
        assert_eq!(rows[0].error.as_deref(), Some("gh failed"));
        // Added but not read yet.
        assert_eq!(rows[1].id, "b");
        assert!(!rows[1].read);
        assert!(rows[1].error.is_none());
        assert!(!rows[2].read, "a loading placeholder is not a read");
    }

    fn row_of(enabled: bool, read: bool, prs: usize, failing: usize, error: Option<&str>) -> Row {
        Row {
            id: "a".into(),
            path: "/w/a".into(),
            name_with_owner: "acme/a".into(),
            default_branch: "main".into(),
            enabled,
            pull_requests: prs,
            failing,
            read,
            error: error.map(Into::into),
        }
    }

    #[test]
    fn the_status_line_says_what_the_last_read_found() {
        let line = |row: &Row| status_line(row);
        assert_eq!(line(&row_of(false, true, 3, 1, None)), ("Paused".into(), Status::Neutral));
        assert_eq!(line(&row_of(true, false, 0, 0, None)), ("Reading…".into(), Status::Neutral));
        assert_eq!(line(&row_of(true, true, 0, 0, None)), ("No open PRs".into(), Status::Neutral));
        assert_eq!(line(&row_of(true, true, 1, 0, None)), ("1 open PR".into(), Status::Neutral));
        assert_eq!(line(&row_of(true, true, 3, 1, None)), ("3 open PRs · 1 failing".into(), Status::Failing));
        assert_eq!(line(&row_of(true, true, 3, 1, Some("gh failed"))), ("gh failed".into(), Status::Failing));
    }

    #[test]
    fn a_tilde_path_starts_at_home() {
        let home = directories::BaseDirs::new().expect("home").home_dir().to_owned();
        assert_eq!(expand_home("~/code/app"), home.join("code/app"));
        assert_eq!(expand_home("/srv/app"), PathBuf::from("/srv/app"));
    }
}
