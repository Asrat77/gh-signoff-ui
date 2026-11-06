//! Repositories: the local checkouts Signoff tracks, whose pull requests
//! each one shows, and adding or removing one.

use std::path::PathBuf;

use egui::{Align, Layout, RichText, Ui};
use signoff_core::config::TrackedRepository;
use signoff_core::{ProductState, PullRequestScope, RepositoryUpdate};
use signoff_ui::theme::space;
use signoff_ui::widgets::{AddButton, Button, ButtonKind, Chip, IconButton, IconButtonKind, spinner, toast};
use signoff_ui::{Status, Type, icons, palette};

use super::{Effects, Settings, parts};

/// What the repositories section keeps between frames.
#[derive(Debug, Default)]
pub(crate) struct Form {
    /// The path typed into the add field.
    path: String,
    /// A checkout being added.
    adding: Option<PathBuf>,
    /// The repository whose remove button was pressed once.
    confirm_remove: Option<String>,
}

pub(super) fn show(ui: &mut Ui, settings: &mut Settings, state: &ProductState, effects: &mut Effects) {
    parts::heading(
        ui,
        "Repositories",
        "Local checkouts of GitHub repositories. Signoff shows their open pull requests and runs checks in them.",
    );
    let rows = rows(settings, state);
    if rows.is_empty() {
        parts::notice(ui, Status::Neutral, "No repository yet. Add a local checkout to start.");
    }
    for repo in &rows {
        row(ui, settings, repo, effects);
    }
    ui.add_space(space::S);
    add(ui, settings);
}

/// One repository as the section draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    id: String,
    path: PathBuf,
    name_with_owner: String,
    default_branch: String,
    enabled: bool,
    scope: PullRequestScope,
    /// A refresh has read it since it was added.
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
                scope: repo.pull_request_scope,
                read: true,
                error: repo.error.clone(),
            })
            .collect(),
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
                scope: repo.pull_request_scope,
                read: read.is_some(),
                error: read.and_then(|r| r.error.clone()),
            }
        })
        .collect()
}

fn row(ui: &mut Ui, settings: &mut Settings, repo: &Row, effects: &mut Effects) {
    let read_only = settings.read_only();
    parts::card(ui, |ui| {
        let p = palette(ui.ctx());
        ui.horizontal(|ui| {
            ui.label(RichText::new(icons::REPOSITORY).font(signoff_ui::fonts::icon(18.0)).color(p.secondary));
            ui.vertical(|ui| {
                ui.label(RichText::new(&repo.name_with_owner).font(Type::Row.font()).color(p.text));
                ui.label(RichText::new(repo.path.display().to_string()).font(Type::Mono.font()).color(p.secondary));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if settings.repositories.confirm_remove.as_deref() == Some(repo.id.as_str()) {
                    if ui.add(Button::new("Remove").kind(ButtonKind::Accent).compact().width(84.0)).clicked() {
                        settings.repositories.confirm_remove = None;
                        remove(ui, settings, repo, effects);
                    }
                    if ui.add(Button::new("Keep").kind(ButtonKind::Secondary).compact().width(72.0)).clicked() {
                        settings.repositories.confirm_remove = None;
                    }
                } else if ui
                    .add_enabled(
                        !read_only,
                        IconButton::new(icons::REMOVE, "Stop tracking").kind(IconButtonKind::OnSheet),
                    )
                    .clicked()
                {
                    settings.repositories.confirm_remove = Some(repo.id.clone());
                }
            });
        });
        ui.add_space(space::S);
        let mut change = None;
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!read_only, |ui| {
                let mine = repo.scope == PullRequestScope::Mine;
                if ui.add(Chip::new("My pull requests").selected(mine)).clicked() {
                    change = Some(Change::Scope(PullRequestScope::Mine));
                }
                if ui.add(Chip::new("Everyone's").selected(!mine)).clicked() {
                    change = Some(Change::Scope(PullRequestScope::All));
                }
                ui.add_space(space::M);
                let (label, icon) = if repo.enabled { ("Watching", icons::CHECK) } else { ("Paused", icons::STOP) };
                let chip = ui.add(Chip::new(label).icon(icon).selected(repo.enabled));
                if chip.on_hover_text("Pause or resume refreshing this repository").clicked() {
                    change = Some(Change::Enabled(!repo.enabled));
                }
            });
        });
        if let Some(change) = change {
            update_repository(ui, settings, &repo.id, |update| match change {
                Change::Scope(scope) => update.pull_request_scope = scope,
                Change::Enabled(enabled) => update.enabled = enabled,
            });
            effects.refresh = true;
        }
        if let Some(error) = &repo.error {
            ui.add_space(space::XS);
            ui.label(RichText::new(error).font(Type::Caption.font()).color(Status::Failing.color(&p)));
        } else if !repo.read && repo.enabled {
            ui.add_space(space::XS);
            ui.label(RichText::new("Reading…").font(Type::Caption.font()).color(p.secondary));
        }
    });
}

/// A change to one of a repository's settings.
#[derive(Clone, Copy)]
enum Change {
    Scope(PullRequestScope),
    Enabled(bool),
}

/// Saves a change to the editable settings of the repository `id`.
pub(super) fn update_repository(ui: &Ui, settings: &Settings, id: &str, change: impl FnOnce(&mut RepositoryUpdate)) {
    let Some(config) = settings.service.config() else { return };
    let Some(current) = config.find_repository(id) else {
        toast(ui.ctx(), "That repository is no longer tracked", Status::Failing);
        return;
    };
    let mut update = RepositoryUpdate {
        enabled: current.enabled,
        pull_request_scope: current.pull_request_scope,
        commands: current.commands,
    };
    change(&mut update);
    if let Err(err) = config.update_repository(id, update) {
        toast(ui.ctx(), err.to_string(), Status::Failing);
    }
}

fn remove(ui: &Ui, settings: &Settings, repo: &Row, effects: &mut Effects) {
    let Some(config) = settings.service.config() else { return };
    match config.remove_repository(&repo.id) {
        Ok(()) => {
            toast(ui.ctx(), format!("Stopped tracking {}", repo.name_with_owner), Status::Neutral);
            effects.refresh = true;
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
            parts::caption(ui, &format!("Checking {} with gh…", path.display()));
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
            start_adding(settings, expand_home(&path));
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
        start_adding(settings, folder);
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

fn start_adding(settings: &mut Settings, path: PathBuf) {
    settings.repositories.adding = Some(path.clone());
    settings.jobs.start(move |service| {
        let result = service.add_repository(&path);
        Box::new(move |settings: &mut Settings, ctx: &egui::Context, effects: &mut Effects| {
            settings.repositories.adding = None;
            match result {
                Ok(repo) => {
                    settings.repositories.path.clear();
                    toast(ctx, format!("Tracking {}", repo.name_with_owner), Status::Ready);
                    effects.refresh = true;
                }
                Err(message) => toast(ctx, message, Status::Failing),
            }
        })
    });
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
            pull_request_scope: PullRequestScope::All,
            ..TrackedRepository::default()
        }
    }

    #[test]
    fn rows_follow_the_config_and_borrow_errors_from_the_last_read() {
        let state = ProductState {
            repositories: vec![
                // Stale: the config says paused and "Everyone's" now.
                RepositoryState {
                    id: "a".into(),
                    enabled: true,
                    pull_request_scope: PullRequestScope::Mine,
                    error: Some("gh failed".into()),
                    ..RepositoryState::default()
                },
                // Removed since: must not appear.
                RepositoryState { id: "gone".into(), ..RepositoryState::default() },
            ],
            ..ProductState::default()
        };
        let rows = merge_rows(&[tracked("a", false), tracked("b", true)], &state);
        assert_eq!(rows.len(), 2);
        assert!(!rows[0].enabled);
        assert_eq!(rows[0].scope, PullRequestScope::All);
        assert!(rows[0].read);
        assert_eq!(rows[0].error.as_deref(), Some("gh failed"));
        // Added but not read yet.
        assert_eq!(rows[1].id, "b");
        assert!(!rows[1].read);
        assert!(rows[1].error.is_none());
    }

    #[test]
    fn a_tilde_path_starts_at_home() {
        let home = directories::BaseDirs::new().expect("home").home_dir().to_owned();
        assert_eq!(expand_home("~/code/app"), home.join("code/app"));
        assert_eq!(expand_home("/srv/app"), PathBuf::from("/srv/app"));
    }
}
