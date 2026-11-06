//! Branch rules: which signoff checks a branch requires before it can be
//! merged. Only the `signoff` and `signoff/*` contexts are changed; reviews,
//! restrictions and every other required check stay as they are.

use egui::{Align, Layout, RichText, Ui};
use signoff_core::checks::valid_context_name;
use signoff_core::rules::BranchChecks;
use signoff_core::{ProductState, RepositoryState, RulesWrite};
use signoff_ui::theme::space;
use signoff_ui::widgets::{Button, ButtonKind, IconButton, IconButtonKind, spinner, toast};
use signoff_ui::{Status, Type, icons, palette};

use super::{Effects, Settings, parts};

/// Reading the branch's protection.
#[derive(Clone, Debug, Default)]
enum Load {
    #[default]
    Loading,
    Loaded(BranchChecks),
    Failed(String),
}

/// The branch rules of one repository while they are edited.
#[derive(Clone, Debug, Default)]
pub(crate) struct Form {
    repo_id: String,
    /// The branch typed in.
    branch: String,
    /// The branch `load` is for.
    loaded_branch: String,
    load: Load,
    /// The check names to require.
    checks: Vec<String>,
    new_check: String,
    saving: bool,
}

impl Form {
    fn new(repo: &RepositoryState) -> Self {
        Self { repo_id: repo.id.clone(), branch: repo.default_branch.clone(), ..Self::default() }
    }

    fn saved_checks(&self) -> Option<&[String]> {
        match &self.load {
            Load::Loaded(read) => Some(&read.checks),
            _ => None,
        }
    }
}

/// In demo mode, the rules come from the sample pull requests.
fn demo_checks(state: &ProductState, repo_id: &str, branch: &str) -> BranchChecks {
    let rules =
        state.pull_requests.iter().find(|pr| pr.repo_id == repo_id && pr.rules.branch == branch).map(|pr| &pr.rules);
    let contexts = rules.map(|rules| rules.contexts.clone()).unwrap_or_default();
    let checks = rules
        .map(|rules| {
            rules.signoff_contexts.iter().map(|c| c.strip_prefix("signoff/").unwrap_or(c).to_owned()).collect()
        })
        .unwrap_or_default();
    BranchChecks { checks, contexts, protected: rules.is_some_and(|rules| rules.protected) }
}

fn start_loading(settings: &mut Settings, state: &ProductState) {
    let Some(form) = &mut settings.rules else { return };
    form.branch = form.branch.trim().to_owned();
    form.loaded_branch = form.branch.clone();
    if settings.service.is_demo() {
        let read = demo_checks(state, &form.repo_id, &form.branch);
        form.checks = read.checks.clone();
        form.load = Load::Loaded(read);
        return;
    }
    form.load = Load::Loading;
    let (repo_id, branch) = (form.repo_id.clone(), form.branch.clone());
    settings.jobs.start(move |service| {
        let result = service.signoff_checks(&repo_id, &branch);
        Box::new(move |settings: &mut Settings, _ctx: &egui::Context, _effects: &mut super::Effects| {
            let Some(form) = &mut settings.rules else { return };
            if form.repo_id != repo_id || form.loaded_branch != branch {
                return;
            }
            match result {
                Ok(read) => {
                    form.checks = read.checks.clone();
                    form.load = Load::Loaded(read);
                }
                Err(message) => form.load = Load::Failed(message),
            }
        })
    });
}

fn start_saving(settings: &mut Settings) {
    let Some(form) = &mut settings.rules else { return };
    form.saving = true;
    let (repo_id, branch, checks) = (form.repo_id.clone(), form.loaded_branch.clone(), form.checks.clone());
    settings.jobs.start(move |service| {
        let result = service.set_signoff_checks(&repo_id, &branch, &checks);
        Box::new(move |settings: &mut Settings, ctx: &egui::Context, effects: &mut Effects| {
            if let Some(form) = &mut settings.rules {
                form.saving = false;
            }
            match result {
                Ok(RulesWrite::Unchanged) => toast(ctx, "Nothing to change", Status::Neutral),
                Ok(_) => {
                    toast(ctx, format!("Saved the rules for {branch}"), Status::Ready);
                    effects.refresh = true;
                    // Read back what GitHub now holds.
                    if let Some(form) = &mut settings.rules
                        && form.repo_id == repo_id
                    {
                        form.load = Load::Loading;
                        reload_after_save(settings, repo_id, branch);
                    }
                }
                Err(message) => toast(ctx, message, Status::Failing),
            }
        })
    });
}

fn reload_after_save(settings: &mut Settings, repo_id: String, branch: String) {
    settings.jobs.start(move |service| {
        let result = service.signoff_checks(&repo_id, &branch);
        Box::new(move |settings: &mut Settings, _ctx: &egui::Context, _effects: &mut Effects| {
            if let Some(form) = &mut settings.rules
                && form.repo_id == repo_id
                && form.loaded_branch == branch
            {
                match result {
                    Ok(read) => {
                        form.checks = read.checks.clone();
                        form.load = Load::Loaded(read);
                    }
                    Err(message) => form.load = Load::Failed(message),
                }
            }
        })
    });
}

pub(super) fn show(ui: &mut Ui, settings: &mut Settings, state: &ProductState, _effects: &mut Effects) {
    parts::heading(
        ui,
        "Branch rules",
        "The signoff checks a branch requires before it can merge. Only signoff checks change here; reviews, \
         restrictions and other required checks stay as they are.",
    );
    let Some(repo) = parts::repository_picker(ui, &mut settings.selected_repo, state) else {
        parts::notice(ui, Status::Neutral, "Add a repository first.");
        return;
    };
    if settings.rules.as_ref().is_none_or(|form| form.repo_id != repo.id) {
        settings.rules = Some(Form::new(repo));
        start_loading(settings, state);
    }

    let mut load = false;
    let mut save = false;
    let read_only = settings.read_only();
    let Some(form) = &mut settings.rules else { return };

    ui.horizontal(|ui| {
        let p = palette(ui.ctx());
        ui.label(RichText::new(icons::BRANCH).font(signoff_ui::fonts::icon(16.0)).color(p.secondary));
        let width = ui.available_width() - 96.0;
        let field = parts::input(ui, &mut form.branch, "Branch, e.g. main", Type::Mono, width);
        let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let changed = form.branch.trim() != form.loaded_branch;
        let button = Button::new(if changed { "Load" } else { "Reload" }).kind(ButtonKind::Secondary).compact();
        if ui.add(button.width(84.0)).clicked() || (entered && changed) {
            load = true;
        }
    });
    ui.add_space(space::M);

    match &form.load {
        Load::Loading => {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(18.0), egui::Sense::hover());
                spinner(ui, rect.center(), 7.0, palette(ui.ctx()).secondary);
                parts::caption(ui, &format!("Reading the protection of {} with gh…", form.loaded_branch));
            });
        }
        Load::Failed(message) => parts::notice(ui, Status::Failing, message),
        Load::Loaded(read) => {
            let read = read.clone();
            save = editor(ui, form, &read, read_only);
        }
    }

    if load {
        start_loading(settings, state);
    } else if save {
        start_saving(settings);
    }
}

/// The checks and the save button; true when save was pressed.
fn editor(ui: &mut Ui, form: &mut Form, read: &BranchChecks, read_only: bool) -> bool {
    let p = palette(ui.ctx());
    if !read.protected {
        parts::notice(
            ui,
            Status::Neutral,
            "This branch has no protection yet. Saving creates it with `gh signoff install`, which requires these checks.",
        );
    }
    parts::label(ui, "Required signoff checks");
    if form.checks.is_empty() {
        parts::caption(ui, "None. Pull requests into this branch can merge without a signoff.");
        ui.add_space(space::S);
    }
    let mut remove = None;
    for (index, check) in form.checks.iter().enumerate() {
        parts::card(ui, |ui| {
            ui.horizontal(|ui| {
                let icon = icons::for_category(signoff_core::checks::category_for(check));
                ui.label(RichText::new(icon).font(signoff_ui::fonts::icon(16.0)).color(p.secondary));
                ui.label(RichText::new(check).font(Type::Row.font()).color(p.text));
                let context = if check == "signoff" { "signoff".to_owned() } else { format!("signoff/{check}") };
                ui.label(RichText::new(context).font(Type::Mono.font()).color(p.secondary));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let button =
                        IconButton::new(icons::REMOVE, "Stop requiring this check").kind(IconButtonKind::OnSheet);
                    if ui.add_enabled(!read_only && !form.saving, button).clicked() {
                        remove = Some(index);
                    }
                });
            });
        });
    }
    if let Some(index) = remove {
        form.checks.remove(index);
    }

    if !read_only {
        ui.horizontal(|ui| {
            let width = ui.available_width() - 96.0;
            let field = parts::input(ui, &mut form.new_check, "Require another check, e.g. lint", Type::Body, width);
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let name = form.new_check.trim().to_owned();
            let valid = valid_context_name(&name) && !form.checks.contains(&name);
            let button = Button::new("Add").kind(ButtonKind::Secondary).compact().width(84.0).enabled(valid);
            if (ui.add(button).clicked() || entered) && valid {
                form.checks.push(name);
                form.checks.sort();
                form.new_check.clear();
            }
        });
    }

    let others: Vec<&String> = read.contexts.iter().filter(|c| !signoff_core::checks::is_signoff_context(c)).collect();
    if !others.is_empty() {
        parts::label(ui, "Also required, kept as they are");
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(icons::LOCKED).font(signoff_ui::fonts::icon(13.0)).color(p.secondary));
            let list = others.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(", ");
            ui.label(RichText::new(list).font(Type::Mono.font()).color(p.secondary));
        });
    }

    ui.add_space(space::M);
    let changed = form.saved_checks().is_some_and(|saved| saved != form.checks.as_slice());
    let label = if form.saving { "Saving…" } else { "Save to GitHub" };
    let button = Button::new(label).kind(ButtonKind::Accent).icon(icons::BRANCH);
    ui.add(button.enabled(changed && !form.saving && !read_only)).clicked()
}

#[cfg(test)]
mod tests {
    use super::*;
    use signoff_core::{PullRequestState, Rules};

    #[test]
    fn demo_rules_come_from_the_sample_pull_requests() {
        let pr = PullRequestState {
            repo_id: "r".into(),
            rules: Rules {
                branch: "main".into(),
                contexts: vec!["ci".into(), "signoff/tests".into()],
                signoff_contexts: vec!["signoff/tests".into()],
                protected: true,
                ..Rules::default()
            },
            ..PullRequestState::default()
        };
        let state = ProductState { pull_requests: vec![pr], ..ProductState::default() };
        let read = demo_checks(&state, "r", "main");
        assert_eq!(read.checks, ["tests"]);
        assert!(read.protected);
        assert_eq!(demo_checks(&state, "r", "dev"), BranchChecks::default());
    }
}
