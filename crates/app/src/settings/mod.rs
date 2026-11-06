//! The settings window: its own native window beside the popover.
//!
//! A column of sections on the left, the chosen one on the right. Edits to
//! the config are small file writes and happen at once; anything that waits
//! on `gh` (adding a checkout, reading or writing branch rules) runs on a
//! background thread through [`Jobs`], so the window never blocks.

mod general;
mod jobs;
mod parts;
mod repositories;

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
}

impl Section {
    const ALL: [Self; 2] = [Self::General, Self::Repositories];

    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Repositories => "Repositories",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::General => icons::GENERAL,
            Self::Repositories => icons::REPOSITORY,
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
        }
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
    let mut effects = Effects::default();
    for done in settings.jobs.finished(ui.ctx()) {
        done(settings, ui.ctx(), &mut effects);
    }
    let p = palette(ui.ctx());
    Frame::new().fill(p.window).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        ui.horizontal_top(|ui| {
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
            ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
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
                        Section::General => general::show(ui, settings, state, &mut effects),
                        Section::Repositories => repositories::show(ui, settings, state, &mut effects),
                    }
                });
            });
        });
    });
    effects
}
