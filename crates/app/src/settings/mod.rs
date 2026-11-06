//! The settings window: its own native window beside the popover.
//!
//! A column of sections on the left, the chosen one on the right. Edits to
//! the config are small file writes and happen at once; anything that waits
//! on `gh` (adding a checkout, reading or writing branch rules) runs on a
//! background thread through [`Jobs`], so the window never blocks.

mod commands;
mod discovery;
mod general;
mod history;
mod jobs;
mod parts;
mod repositories;
mod rules;

use std::sync::Arc;

use egui::{Frame, Margin, RichText, ScrollArea, Ui, ViewportBuilder, ViewportId};
use signoff_core::{ProductService, ProductState};
use signoff_ui::theme::space;
use signoff_ui::widgets::{Chip, show_toasts};
use signoff_ui::{Type, icons, palette};

pub(crate) use jobs::Jobs;

use crate::app::Signoff;

/// The sections, in the order the column lists them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Section {
    /// Launch at login, refresh interval, the environment.
    #[default]
    General,
    /// The tracked checkouts.
    Repositories,
    /// The local command behind each check.
    Commands,
    /// The signoff checks a branch requires.
    BranchRules,
    /// Local runs and signoffs.
    History,
}

impl Section {
    const ALL: [Self; 5] = [Self::General, Self::Repositories, Self::Commands, Self::BranchRules, Self::History];

    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Repositories => "Repositories",
            Self::Commands => "Commands",
            Self::BranchRules => "Branch rules",
            Self::History => "History",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::General => icons::GENERAL,
            Self::Repositories => icons::REPOSITORY,
            Self::Commands => icons::TERMINAL,
            Self::BranchRules => icons::BRANCH,
            Self::History => icons::HISTORY,
        }
    }
}

/// What the settings window keeps between frames.
pub(crate) struct Settings {
    service: Arc<ProductService>,
    jobs: Jobs,
    pub(crate) section: Section,
    /// Whether Signoff starts at login, read when the window first opens.
    launch_at_login: Option<bool>,
    repositories: repositories::Form,
    discovery: discovery::Discovery,
    /// The repository the commands and branch rules sections show.
    selected_repo: Option<String>,
    commands: Option<commands::Draft>,
    /// A repository and check Commands should show on its next frame.
    configure: Option<(String, String)>,
    rules: Option<rules::Form>,
}

/// What the settings asked the app for this frame.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Effects {
    /// The config changed: read GitHub again.
    pub(crate) refresh: bool,
}

impl Settings {
    /// Settings for `service`, waking the UI through `wake` when background
    /// work finishes.
    pub(crate) fn new(service: Arc<ProductService>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let jobs = Jobs::new(service.clone(), wake);
        Self {
            service,
            jobs,
            section: Section::default(),
            launch_at_login: None,
            repositories: repositories::Form::default(),
            discovery: discovery::Discovery::default(),
            selected_repo: None,
            commands: None,
            configure: None,
            rules: None,
        }
    }

    /// Opens Branch rules on `branch` of the repository `repo_id` with the
    /// `signoff` check ready to save: the way to set signoff up on a
    /// repository. The caller opens the window.
    pub(crate) fn set_up_signoff(&mut self, repo_id: &str, branch: &str) {
        self.section = Section::BranchRules;
        self.selected_repo = Some(repo_id.to_owned());
        rules::set_up(self, repo_id, branch);
    }

    /// Opens Commands on the repository `repo_id` with a row for `check`
    /// and its command field focused: the way to map a local command to a
    /// required check. The caller opens the window.
    pub(crate) fn configure_check(&mut self, repo_id: &str, check: &str) {
        self.section = Section::Commands;
        self.selected_repo = Some(repo_id.to_owned());
        self.configure = Some((repo_id.to_owned(), check.to_owned()));
    }

    /// Asks for a checkout with the folder picker and starts tracking it.
    pub(crate) fn choose_and_add(&mut self, ctx: &egui::Context) {
        repositories::choose_and_add(ctx, self);
    }

    /// The checkouts found on this computer, with Add buttons; draws nothing
    /// when there are none. `compact` caps the list for a small screen.
    pub(crate) fn show_suggestions(&mut self, ui: &mut Ui, compact: bool) {
        discovery::show(ui, self, compact);
    }

    /// Scans under `home` for suggestions instead of the real home folder.
    #[cfg(test)]
    pub(crate) fn find_checkouts_in(&mut self, home: Option<std::path::PathBuf>) {
        self.discovery = discovery::Discovery::scan_under(home);
    }

    /// Shows `found` as the suggestions, as if a scan had returned them.
    #[cfg(test)]
    pub(crate) fn suggest(&mut self, found: Vec<signoff_core::discover::Found>) {
        self.discovery = discovery::Discovery::with_found(found);
    }

    /// Tracks the folders dropped onto the window this frame.
    pub(crate) fn accept_drops(&mut self, ctx: &egui::Context) {
        repositories::accept_drops(ctx, self);
    }

    /// The checkout being added, while `gh` checks it.
    pub(crate) fn adding(&self) -> Option<&std::path::Path> {
        repositories::adding(self)
    }

    /// Applies the background jobs that finished. The popover runs this
    /// too, so an add started there completes with Settings closed.
    pub(crate) fn pump(&mut self, ctx: &egui::Context) -> Effects {
        let mut effects = Effects::default();
        for done in self.jobs.finished(ctx) {
            done(self, ctx, &mut effects);
        }
        effects
    }

    /// Waits, for a bounded time, until config and history are on disk.
    pub(crate) fn flush(&self) {
        self.service.flush();
    }

    /// Whether edits are refused: demo data has no config to change.
    pub(crate) fn read_only(&self) -> bool {
        self.service.is_demo()
    }
}

/// Draws the settings window while `app.settings_open`.
pub(crate) fn window(ctx: &egui::Context, app: &mut Signoff) {
    let id = ViewportId::from_hash_of("settings");
    let builder = ViewportBuilder::default()
        .with_title("Signoff Settings")
        .with_app_id("signoff-settings")
        .with_inner_size([760.0, 560.0])
        .with_min_inner_size([600.0, 420.0]);
    let mut effects = Effects::default();
    ctx.show_viewport_immediate(id, builder, |ui, _class| {
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            app.settings_open = false;
        }
        effects = contents(ui, &mut app.settings, &app.state);
        show_toasts(ui.ctx());
    });
    if effects.refresh {
        app.refresh();
    }
}

/// The window's contents. Split out so tests can draw it without a window.
pub(crate) fn contents(ui: &mut Ui, settings: &mut Settings, state: &ProductState) -> Effects {
    let mut effects = settings.pump(ui.ctx());
    let p = palette(ui.ctx());
    Frame::new().fill(p.window).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        ui.horizontal_top(|ui| {
            // Each column stacks its own contents top to bottom; inside the
            // row they would otherwise run sideways.
            ui.vertical(|ui| nav(ui, settings));
            ui.vertical(|ui| {
                ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    section(ui, settings, state, &mut effects);
                });
            });
        });
    });
    effects
}

/// The column of sections.
fn nav(ui: &mut Ui, settings: &mut Settings) {
    let p = palette(ui.ctx());
    Frame::new().inner_margin(Margin::same(18)).show(ui, |ui| {
        ui.set_width(170.0);
        ui.set_min_height(ui.available_height());
        ui.label(RichText::new("Settings").font(Type::Title.font()).color(p.text));
        ui.add_space(space::L);
        for section in Section::ALL {
            let chip = Chip::new(section.label()).icon(section.icon()).selected(settings.section == section);
            if ui.add(chip.width(170.0)).clicked() {
                settings.section = section;
            }
            ui.add_space(space::XS);
        }
    });
}

/// The chosen section.
fn section(ui: &mut Ui, settings: &mut Settings, state: &ProductState, effects: &mut Effects) {
    Frame::new().inner_margin(Margin { left: 6, right: 22, top: 22, bottom: 22 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if settings.read_only() {
            parts::notice(
                ui,
                signoff_ui::Status::Neutral,
                "Demo data: settings are shown but nothing is saved or sent to GitHub.",
            );
        }
        match settings.section {
            Section::General => general::show(ui, settings, state, effects),
            Section::Repositories => repositories::show(ui, settings, state, effects),
            Section::Commands => commands::show(ui, settings, state, effects),
            Section::BranchRules => rules::show(ui, settings, state, effects),
            Section::History => history::show(ui, state),
        }
    });
}
