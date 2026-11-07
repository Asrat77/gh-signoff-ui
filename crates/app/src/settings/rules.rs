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
    /// Read the branch as soon as the section draws: set when another part
    /// of the app opened this form.
    reload: bool,
    /// Add the `signoff` check once the branch is read: "Set up signoff".
    want_signoff: bool,
}

/// Adds the bare `signoff` check to `checks`, keeping them sorted. False
/// when it was already there.
fn prefill_signoff(checks: &mut Vec<String>) -> bool {
    if checks.iter().any(|check| check == "signoff") {
        return false;
    }
    checks.push("signoff".to_owned());
    checks.sort();
    true
}

impl Form {
    fn new(repo: &RepositoryState) -> Self {
        Self { repo_id: repo.id.clone(), branch: repo.default_branch.clone(), ..Self::default() }
    }

    /// A form for `branch` of the repository `repo_id`, opened to set up
    /// signoff: it reads the branch, then has the `signoff` check ready.
    fn set_up(repo_id: &str, branch: &str) -> Self {
        Self {
            repo_id: repo_id.to_owned(),
            branch: branch.to_owned(),
            reload: true,
            want_signoff: true,
            ..Self::default()
        }
    }

    /// Takes in what GitHub holds. The first read after a set-up request
    /// also adds `signoff` to what is required, ready to save.
    fn take(&mut self, read: BranchChecks) {
        self.checks = read.checks.clone();
        if std::mem::take(&mut self.want_signoff) {
            prefill_signoff(&mut self.checks);
        }
        self.load = Load::Loaded(read);
    }
}

/// The status context a check name stands for: `signoff` stays bare, any
/// other name is `signoff/<name>`.
fn context(check: &str) -> String {
    if check == "signoff" { check.to_owned() } else { format!("signoff/{check}") }
}

/// `a`, `a and b`, `a, b and c`, each in backticks.
fn listed(checks: &[&String]) -> String {
    let names: Vec<String> = checks.iter().map(|check| format!("`{}`", context(check))).collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What `branch` of `repo` requires on GitHub now, in a sentence, and how
/// it reads: set up, or not yet.
fn current_state(repo: &str, branch: &str, read: &BranchChecks) -> (Status, String) {
    let repo = repo.rsplit('/').next().unwrap_or(repo);
    if !read.protected {
        let text =
            format!("{branch} has no branch protection yet, so pull requests into it can merge without a signoff.");
        return (Status::Neutral, text);
    }
    if read.checks.is_empty() {
        let text = format!("{branch} is protected, but {repo} requires no signoff on it yet.");
        return (Status::Neutral, text);
    }
    let checks: Vec<&String> = read.checks.iter().collect();
    (Status::Ready, format!("{repo} already requires {} on {branch}.", listed(&checks)))
}

/// What "Save to GitHub" would change about what `branch` requires, one
/// line each, going from what GitHub holds (`read`) to `checks`. Empty
/// when nothing would change.
fn changes(branch: &str, read: &BranchChecks, checks: &[String]) -> Vec<String> {
    let added: Vec<&String> = checks.iter().filter(|check| !read.checks.contains(check)).collect();
    let removed: Vec<&String> = read.checks.iter().filter(|check| !checks.contains(check)).collect();
    if added.is_empty() && removed.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    if !read.protected && !added.is_empty() {
        lines.push(format!("Turn on branch protection for {branch}, with `gh signoff install`."));
    }
    if !added.is_empty() {
        lines.push(format!(
            "Require {} before a pull request into {branch} can merge. Someone signs off its head commit after \
             running the check locally.",
            listed(&added)
        ));
    }
    if !removed.is_empty() {
        lines.push(format!("Stop requiring {}.", listed(&removed)));
    }
    if read.protected {
        lines.push("Keep reviews, push rules and every other required check as they are.".to_owned());
    }
    lines
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
                Ok(read) => form.take(read),
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

/// Opens the form for `branch` of `repo_id` with `signoff` ready to save.
pub(super) fn set_up(settings: &mut Settings, repo_id: &str, branch: &str) {
    settings.rules = Some(Form::set_up(repo_id, branch));
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
    } else if settings.rules.as_mut().is_some_and(|form| std::mem::take(&mut form.reload)) {
        start_loading(settings, state);
    }

    let repo_name = repo.name_with_owner.clone();
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
            save = editor(ui, form, &repo_name, &read, read_only);
        }
    }

    if load {
        start_loading(settings, state);
    } else if save {
        start_saving(settings);
    }
}

/// Where the branch stands, the checks, what saving changes and the save
/// button; true when save was pressed.
fn editor(ui: &mut Ui, form: &mut Form, repo: &str, read: &BranchChecks, read_only: bool) -> bool {
    let p = palette(ui.ctx());
    let (status, now) = current_state(repo, &form.loaded_branch, read);
    parts::notice(ui, status, &now);
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
                ui.label(RichText::new(context(check)).font(Type::Mono.font()).color(p.secondary));
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
    let changes = changes(&form.loaded_branch, read, &form.checks);
    if !changes.is_empty() {
        parts::label(ui, "Save to GitHub will");
        for line in &changes {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = space::S;
                ui.label(RichText::new("•").font(Type::Caption.font()).color(p.secondary));
                ui.add(egui::Label::new(RichText::new(line).font(Type::Caption.font()).color(p.text)).wrap());
            });
        }
        parts::caption(ui, "Nothing changes on GitHub until you save.");
        ui.add_space(space::S);
    }
    let label = if form.saving { "Saving…" } else { "Save to GitHub" };
    let button = Button::new(label).kind(ButtonKind::Accent).icon(icons::BRANCH);
    ui.add(button.enabled(!changes.is_empty() && !form.saving && !read_only)).clicked()
}

#[cfg(test)]
mod tests {
    use super::*;
    use signoff_core::{PullRequestState, Rules};

    #[test]
    fn signoff_is_prefilled_once_and_kept_sorted() {
        let mut checks = vec!["tests".to_owned()];
        assert!(prefill_signoff(&mut checks));
        assert_eq!(checks, ["signoff", "tests"]);
        assert!(!prefill_signoff(&mut checks), "already required");
        assert_eq!(checks, ["signoff", "tests"]);
    }

    fn read(protected: bool, checks: &[&str]) -> BranchChecks {
        BranchChecks {
            checks: checks.iter().map(|c| (*c).to_owned()).collect(),
            contexts: checks.iter().map(|c| context(c)).collect(),
            protected,
        }
    }

    fn owned(checks: &[&str]) -> Vec<String> {
        checks.iter().map(|c| (*c).to_owned()).collect()
    }

    #[test]
    fn the_current_state_says_when_signoff_is_already_set_up() {
        let (status, text) = current_state("Asrat77/gh-signoff-ui", "main", &read(true, &["signoff"]));
        assert_eq!(status, Status::Ready);
        assert_eq!(text, "gh-signoff-ui already requires `signoff` on main.");

        let (_, text) = current_state("o/r", "main", &read(true, &["lint", "signoff", "tests"]));
        assert_eq!(text, "r already requires `signoff/lint`, `signoff` and `signoff/tests` on main.");
    }

    #[test]
    fn the_current_state_says_when_nothing_is_required() {
        let (status, text) = current_state("o/r", "main", &read(true, &[]));
        assert_eq!(status, Status::Neutral);
        assert!(text.contains("requires no signoff"), "{text}");
        let (status, text) = current_state("o/r", "dev", &read(false, &[]));
        assert_eq!(status, Status::Neutral);
        assert!(text.starts_with("dev has no branch protection"), "{text}");
    }

    #[test]
    fn saving_explains_what_it_adds_and_keeps() {
        let lines = changes("main", &read(true, &["tests"]), &owned(&["signoff", "tests"]));
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].starts_with("Require `signoff` before a pull request into main can merge."), "{lines:?}");
        assert!(lines[1].starts_with("Keep reviews"), "{lines:?}");
    }

    #[test]
    fn saving_an_unprotected_branch_says_it_turns_protection_on() {
        let lines = changes("main", &read(false, &[]), &owned(&["signoff"]));
        assert!(lines[0].contains("Turn on branch protection for main"), "{lines:?}");
        assert!(lines[1].starts_with("Require `signoff`"), "{lines:?}");
        assert_eq!(lines.len(), 2, "nothing else to keep: {lines:?}");
    }

    #[test]
    fn saving_says_what_it_stops_requiring() {
        let lines = changes("main", &read(true, &["lint", "signoff"]), &owned(&["signoff"]));
        assert_eq!(lines[0], "Stop requiring `signoff/lint`.");
    }

    #[test]
    fn nothing_to_save_when_the_checks_match() {
        assert!(changes("main", &read(true, &["signoff"]), &owned(&["signoff"])).is_empty());
        assert!(changes("main", &read(false, &[]), &[]).is_empty());
    }

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
