//! Commands: the local command each signoff check runs before Signoff
//! writes its signoff, per repository.

use std::collections::{BTreeMap, BTreeSet};

use egui::{Align, DragValue, Layout, RichText, Ui};
use signoff_core::checks::valid_context_name;
use signoff_core::ci::{WHOLE, missing};
use signoff_core::config::{DEFAULT_TIMEOUT_SECONDS, MAX_TIMEOUT_SECONDS};
use signoff_core::suggest;
use signoff_core::{CommandMapping, CommandSuggestion, DetectedCommand, ProductService, ProductState, RepositoryState};
use signoff_ui::theme::space;
use signoff_ui::widgets::{Button, ButtonKind, Chip, IconButton, IconButtonKind, toast};
use signoff_ui::{Status, Type, icons, palette};

use super::{Effects, Settings, parts, repositories};

/// One check's command while it is edited.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    check: String,
    command: String,
    minutes: u32,
}

/// The commands of one repository while they are edited.
#[derive(Clone, Debug, Default)]
pub(crate) struct Draft {
    repo_id: String,
    rows: Vec<Row>,
    /// As saved, to tell whether anything changed.
    saved: Vec<Row>,
    /// The name typed into the add field.
    new_check: String,
    /// The check whose command field takes focus on the next frame.
    focus: Option<String>,
    /// Commands the checkout suggests, read when the draft is made.
    suggested: Vec<CommandSuggestion>,
    /// The commands Signoff set up from the CI config, by check.
    detected: BTreeMap<String, DetectedCommand>,
    /// What the default branch's CI runs for the whole pull request, once
    /// read; empty when CI has nothing to say.
    ci: Option<BTreeMap<String, DetectedCommand>>,
    /// CI has been asked, so it is not asked on every frame.
    ci_requested: bool,
    /// CI's commands the person picked in place of their own, to record as
    /// Signoff's once saved.
    use_ci: BTreeMap<String, DetectedCommand>,
    /// "Detect from CI again" is running.
    detecting: bool,
}

/// How many suggested commands show under a command field.
const CHIPS: usize = 5;

fn rows(commands: &BTreeMap<String, CommandMapping>) -> Vec<Row> {
    commands
        .iter()
        .map(|(check, mapping)| Row {
            check: check.clone(),
            command: mapping.command.clone(),
            minutes: mapping.timeout_seconds.div_ceil(60).max(1),
        })
        .collect()
}

/// The commands to save from `rows`, or why they cannot be saved. Rows
/// without a command are dropped: the check then has no local command.
fn commands(rows: &[Row]) -> Result<BTreeMap<String, CommandMapping>, String> {
    let mut out = BTreeMap::new();
    for row in rows.iter().filter(|row| !row.command.trim().is_empty()) {
        let check = row.check.trim();
        if !valid_context_name(check) {
            return Err(format!("\"{check}\" is not a valid check name: use letters, digits, '-', '_' and '.'"));
        }
        let mapping = CommandMapping {
            command: row.command.trim().to_owned(),
            timeout_seconds: (row.minutes.max(1) * 60).min(MAX_TIMEOUT_SECONDS),
        };
        if out.insert(check.to_owned(), mapping).is_some() {
            return Err(format!("\"{check}\" is listed twice"));
        }
    }
    Ok(out)
}

/// Check names this repository's pull requests require that have no row
/// yet, to offer as one-click additions.
fn suggestions(state: &ProductState, repo_id: &str, rows: &[Row]) -> Vec<String> {
    let have: BTreeSet<&str> = rows.iter().map(|row| row.check.as_str()).collect();
    state
        .pull_requests
        .iter()
        .filter(|pr| pr.repo_id == repo_id)
        .flat_map(|pr| pr.checks.iter().filter(|check| check.required).map(|check| check.name.clone()))
        .filter(|name| !have.contains(name.as_str()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(super) fn show(ui: &mut Ui, settings: &mut Settings, state: &ProductState, _: &mut Effects) {
    parts::heading(
        ui,
        "Commands",
        "What runs on your computer before Signoff signs off a check. Each runs in the checkout, on the pull \
         request's head commit, and the signoff is written only when it exits 0. Signoff sets up what it can \
         from the default branch's CI config; edit or remove any of them and it leaves that check to you.",
    );
    let Some(repo) = parts::repository_picker(ui, &mut settings.selected_repo, state) else {
        parts::notice(ui, Status::Neutral, "Add a repository first.");
        return;
    };
    let (saved_commands, detected) = saved(&settings.service, repo);
    let saved_rows = rows(&saved_commands);
    let draft = match &mut settings.commands {
        // Untouched drafts follow the config, which detection may change.
        Some(draft) if draft.repo_id == repo.id && (draft.rows != draft.saved || draft.saved == saved_rows) => draft,
        slot => {
            let suggested = suggest::detect(&repo.path);
            // CI's answer outlives a draft rebuilt for a config change.
            let before = slot.take().filter(|draft| draft.repo_id == repo.id);
            let (ci, ci_requested, detecting) =
                before.map_or((None, false, false), |d| (d.ci, d.ci_requested, d.detecting));
            slot.insert(Draft {
                repo_id: repo.id.clone(),
                saved: saved_rows.clone(),
                rows: saved_rows,
                new_check: String::new(),
                focus: None,
                suggested,
                detected,
                ci,
                ci_requested,
                use_ci: BTreeMap::new(),
                detecting,
            })
        }
    };
    if let Some((_, check)) = settings.configure.take_if(|(id, _)| *id == repo.id) {
        if !draft.rows.iter().any(|row| row.check == check) {
            draft.rows.push(Row {
                check: check.clone(),
                command: String::new(),
                minutes: DEFAULT_TIMEOUT_SECONDS / 60,
            });
        }
        draft.focus = Some(check);
    }
    let read_only = settings.service.is_demo();

    let mut detect = false;
    ui.horizontal(|ui| {
        let label = if draft.detecting { "Reading CI…" } else { "Detect from CI again" };
        let button = Button::new(label).kind(ButtonKind::Inset).compact().width(168.0);
        let found = ui.add_enabled(!read_only && !draft.detecting, button).on_hover_text(
            "Read the default branch's CI config again and set up a command for each check that has none, \
             including ones you removed. Commands you typed stay.",
        );
        detect = found.clicked();
    });
    ui.add_space(space::S);
    if draft.rows.is_empty() {
        parts::notice(ui, Status::Neutral, "No commands yet. Add a check below, such as tests or lint.");
    }
    let mut remove = None;
    let mut focus = draft.focus.take();
    for (index, row) in draft.rows.iter_mut().enumerate() {
        parts::card(ui, |ui| {
            let p = palette(ui.ctx());
            ui.horizontal(|ui| {
                let icon = icons::for_category(signoff_core::checks::category_for(&row.check));
                ui.label(RichText::new(icon).font(signoff_ui::fonts::icon(16.0)).color(p.secondary));
                ui.label(RichText::new(&row.check).font(Type::Row.font()).color(p.text));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let button = IconButton::new(icons::REMOVE, "Remove this command").kind(IconButtonKind::OnSheet);
                    if ui.add_enabled(!read_only, button).clicked() {
                        remove = Some(index);
                    }
                    ui.add_enabled(
                        !read_only,
                        DragValue::new(&mut row.minutes).range(1..=MAX_TIMEOUT_SECONDS / 60).suffix(" min"),
                    )
                    .on_hover_text("Stop the command after this long");
                    ui.label(RichText::new("Timeout").font(Type::Caption.font()).color(p.secondary));
                });
            });
            ui.add_space(space::XS);
            ui.add_enabled_ui(!read_only, |ui| {
                let field =
                    parts::input(ui, &mut row.command, "e.g. cargo test --workspace", Type::Mono, ui.available_width());
                if focus.as_deref() == Some(row.check.as_str()) {
                    focus = None;
                    field.request_focus();
                    field.scroll_to_me(Some(Align::Center));
                }
            });
            let p = palette(ui.ctx());
            if let Some(found) = draft.detected.get(&row.check).filter(|found| found.command == row.command) {
                ui.add_space(space::XS);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(icons::DETECTED).font(signoff_ui::fonts::icon(12.0)).color(p.secondary));
                    let text = format!("Set up from {}", found.source);
                    ui.label(RichText::new(text).font(Type::Caption.font()).color(p.secondary));
                });
            } else if !row.command.trim().is_empty() {
                ui.add_space(space::XS);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(icons::TERMINAL).font(signoff_ui::fonts::icon(12.0)).color(p.secondary));
                    ui.label(RichText::new("Typed by you").font(Type::Caption.font()).color(p.secondary));
                });
                if let Some(ci) = draft.ci.as_ref().and_then(|ci| ci.get(WHOLE)).filter(|_| row.check == WHOLE) {
                    let left_out = missing(&row.command, &ci.command);
                    if !left_out.is_empty() {
                        let button = less_than_ci(ui, &left_out, &ci.source, read_only);
                        if button {
                            row.command.clone_from(&ci.command);
                            draft.use_ci.insert(row.check.clone(), ci.clone());
                        }
                    }
                }
            }
            if !read_only && row.command.trim().is_empty() {
                let chosen = suggestion_chips(ui, &draft.suggested, &row.check);
                if let Some(command) = chosen {
                    row.command = command;
                }
            }
        });
    }
    if let Some(index) = remove {
        draft.rows.remove(index);
    }

    if !read_only {
        add_row(ui, draft, state, &repo.id);
    }
    ui.add_space(space::M);
    let changed = draft.rows != draft.saved;
    let save = Button::new("Save commands").kind(ButtonKind::Primary).enabled(changed && !read_only);
    let mut to_save = None;
    let use_ci = draft.use_ci.clone();
    if ui.add(save).clicked() {
        match commands(&draft.rows) {
            Ok(commands) => to_save = Some(commands),
            Err(message) => toast(ui.ctx(), message, Status::Failing),
        }
    }
    if let Some(commands) = to_save {
        repositories::update_repository(ui, settings, &repo.id, |update| update.commands = commands);
        if !use_ci.is_empty()
            && let Some(config) = settings.service.config()
            && let Err(err) = config.mark_detected(&repo.id, &use_ci)
        {
            toast(ui.ctx(), err.to_string(), Status::Failing);
        }
        settings.commands = None;
        // Applied to the shown checks by the config subscription; GitHub
        // has nothing new to say.
        toast(ui.ctx(), "Commands saved", Status::Ready);
        return;
    }
    start_jobs(settings, &repo.id, detect);
}

/// Whether CI has been asked what it runs for the whole pull request.
#[cfg(test)]
pub(super) fn ci_known(settings: &Settings) -> bool {
    settings.commands.as_ref().is_some_and(|draft| draft.ci.is_some())
}

/// Shows `found` as CI's answer for the whole pull request.
#[cfg(test)]
pub(super) fn set_ci(settings: &mut Settings, found: BTreeMap<String, DetectedCommand>) {
    if let Some(draft) = &mut settings.commands {
        draft.ci = Some(found);
    }
}

/// Starts the background reads this frame asked for: CI's command for the
/// whole pull request when a `signoff` row can be compared with it, and
/// "Detect from CI again".
fn start_jobs(settings: &mut Settings, repo_id: &str, detect: bool) {
    let Some(draft) = settings.commands.as_mut().filter(|draft| draft.repo_id == repo_id) else { return };
    let compare = !draft.ci_requested && draft.rows.iter().any(|row| row.check == WHOLE);
    if detect {
        draft.detecting = true;
    }
    draft.ci_requested |= compare;
    let repo_id = repo_id.to_owned();
    if compare {
        let repo_id = repo_id.clone();
        settings.jobs.start(move |service| {
            let found = service.ci_commands(&repo_id, &[WHOLE.to_owned()]);
            Box::new(move |settings: &mut Settings, _: &egui::Context, _: &mut Effects| {
                if let Some(draft) = settings.commands.as_mut().filter(|draft| draft.repo_id == repo_id) {
                    draft.ci = Some(found);
                }
            })
        });
    }
    if detect {
        settings.jobs.start(move |service| {
            let changed = service.detect_commands_again(&repo_id);
            Box::new(move |settings: &mut Settings, ctx: &egui::Context, _: &mut Effects| {
                if let Some(draft) = settings.commands.as_mut().filter(|draft| draft.repo_id == repo_id) {
                    draft.detecting = false;
                    // Compare with what CI says now.
                    draft.ci = None;
                    draft.ci_requested = false;
                }
                let (text, status) = if changed {
                    ("Set up commands from CI", Status::Ready)
                } else {
                    ("CI has nothing new to set up", Status::Neutral)
                };
                toast(ctx, text, status);
            })
        });
    }
}

/// Says a hand-typed `signoff` command runs less than CI does, and offers
/// CI's. Returns whether that was clicked.
fn less_than_ci(ui: &mut Ui, left_out: &[String], source: &str, read_only: bool) -> bool {
    let p = palette(ui.ctx());
    let status = Status::Attention;
    ui.add_space(space::XS);
    ui.horizontal_top(|ui| {
        ui.label(RichText::new(status.icon()).font(signoff_ui::fonts::icon(13.0)).color(status.color(&p)));
        let text = format!(
            "CI also runs {}. A signoff from this command vouches for less than CI checks. See {source}.",
            left_out.iter().map(|step| format!("`{step}`")).collect::<Vec<_>>().join(", "),
        );
        ui.add(egui::Label::new(RichText::new(text).font(Type::Caption.font()).color(p.text)).wrap());
    });
    ui.add_space(space::XS);
    let button = Button::new("Use CI's commands").kind(ButtonKind::Inset).compact().width(150.0);
    ui.add_enabled(!read_only, button).on_hover_text("Replace this command with everything CI runs").clicked()
}

/// The commands as saved, and those of them Signoff set up: from the
/// config, or the demo state's.
fn saved(
    service: &ProductService,
    repo: &RepositoryState,
) -> (BTreeMap<String, CommandMapping>, BTreeMap<String, DetectedCommand>) {
    service
        .config()
        .and_then(|config| config.find_repository(&repo.id))
        .map_or_else(|| (repo.commands.clone(), BTreeMap::new()), |tracked| (tracked.commands, tracked.detected))
}

/// Clickable commands from the checkout, best match for `check` first;
/// returns the one clicked.
fn suggestion_chips(ui: &mut Ui, suggested: &[CommandSuggestion], check: &str) -> Option<String> {
    let ranked = suggest::rank(suggested, check);
    if ranked.is_empty() {
        return None;
    }
    let mut chosen = None;
    ui.add_space(space::XS);
    ui.horizontal_wrapped(|ui| {
        for suggestion in ranked.into_iter().take(CHIPS) {
            let chip = Chip::new(&suggestion.command).icon(icons::TERMINAL);
            if ui.add(chip).on_hover_text(format!("From {}", suggestion.source)).clicked() {
                chosen = Some(suggestion.command.clone());
            }
        }
    });
    chosen
}

fn add_row(ui: &mut Ui, draft: &mut Draft, state: &ProductState, repo_id: &str) {
    let suggested = suggestions(state, repo_id, &draft.rows);
    let mut added = None;
    if !suggested.is_empty() {
        parts::label(ui, "Required by this repository's pull requests");
        ui.horizontal_wrapped(|ui| {
            for name in &suggested {
                if ui.add(Chip::new(name).icon(icons::ADD)).clicked() {
                    added = Some(name.clone());
                }
            }
        });
        ui.add_space(space::S);
    }
    ui.horizontal(|ui| {
        let width = ui.available_width() - 96.0;
        let field = parts::input(ui, &mut draft.new_check, "Another check, e.g. security", Type::Body, width);
        let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let name = draft.new_check.trim().to_owned();
        let valid = valid_context_name(&name) && !draft.rows.iter().any(|row| row.check == name);
        let button = Button::new("Add").kind(ButtonKind::Secondary).compact().width(84.0).enabled(valid);
        if (ui.add(button).clicked() || entered) && valid {
            added = Some(name);
            draft.new_check.clear();
        }
    });
    if let Some(check) = added {
        draft.rows.push(Row { check, command: String::new(), minutes: DEFAULT_TIMEOUT_SECONDS / 60 });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(check: &str, command: &str, minutes: u32) -> Row {
        Row { check: check.into(), command: command.into(), minutes }
    }

    #[test]
    fn rows_without_a_command_are_dropped() {
        let saved = commands(&[row("tests", " cargo test ", 2), row("lint", "  ", 5)]).expect("valid");
        assert_eq!(saved.len(), 1);
        assert_eq!(saved["tests"], CommandMapping { command: "cargo test".into(), timeout_seconds: 120 });
    }

    #[test]
    fn bad_names_and_duplicates_are_refused() {
        assert!(commands(&[row("-f", "x", 1)]).is_err());
        assert!(commands(&[row("tests", "a", 1), row("tests", "b", 1)]).is_err());
    }

    #[test]
    fn timeouts_round_up_to_whole_minutes() {
        let mapping = BTreeMap::from([("t".to_owned(), CommandMapping { command: "x".into(), timeout_seconds: 90 })]);
        assert_eq!(rows(&mapping)[0].minutes, 2);
    }
}
