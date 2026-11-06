//! General: launch at login, how Signoff keeps up to date, and what it
//! found on this computer.
//!
//! There is no refresh interval to set: the worker picks its own cadence
//! (`crate::cadence`), and `refreshSeconds` in older configs is ignored.

use egui::{RichText, Ui};
use signoff_core::ProductState;
use signoff_ui::theme::space;
use signoff_ui::widgets::{Button, ButtonKind, ToggleRow, toast};
use signoff_ui::{Status, Type, palette};

use super::{Effects, Settings, parts};
use crate::{autostart, hotkey};

pub(super) fn show(ui: &mut Ui, settings: &mut Settings, state: &ProductState, effects: &mut Effects) {
    parts::heading(ui, "General", "How Signoff starts and keeps up to date.");

    let mut launch = *settings.launch_at_login.get_or_insert_with(autostart::is_enabled);
    let hint = "Start Signoff in the tray when you sign in.";
    if ui.add(ToggleRow::new(&mut launch, "Launch at login", hint)).changed() {
        match autostart::set_enabled(launch) {
            Ok(()) => settings.launch_at_login = Some(launch),
            Err(message) => toast(ui.ctx(), message, Status::Failing),
        }
    }
    ui.add_space(space::S);

    notifications(ui, settings);
    ui.add_space(space::S);

    shortcut(ui, settings, effects);
    ui.add_space(space::S);

    updates(ui);
    environment(ui, state, settings.installing, !settings.read_only(), effects);
}

fn notifications(ui: &mut Ui, settings: &Settings) {
    let mut on = settings.notifications_on();
    let hint =
        "When the popover is hidden: one of your pull requests starts failing or is ready to merge, or a run finishes.";
    let toggle = ui.add_enabled(!settings.read_only(), ToggleRow::new(&mut on, "Notifications", hint));
    if toggle.changed()
        && let Some(config) = settings.service.config()
        && let Err(err) = config.set_notifications(on)
    {
        toast(ui.ctx(), err.to_string(), Status::Failing);
    }
}

/// How Signoff keeps up to date. Nothing to set: it reads as often as what
/// is on screen needs.
fn updates(ui: &mut Ui) {
    parts::card(ui, |ui| {
        let p = palette(ui.ctx());
        ui.label(RichText::new("Updates").font(Type::Row.font()).color(p.text));
        ui.add_space(space::XS);
        parts::caption(
            ui,
            "Signoff reads GitHub every few seconds while the popover is open or a check is pending, about once a minute in the background, and at once when you refresh or check out a branch. It reads less when nothing is open, and backs off while GitHub can't be reached.",
        );
    });
}

fn shortcut(ui: &mut Ui, settings: &Settings, effects: &mut Effects) {
    let (mut on, chosen) = settings.shortcut_config();
    let current = hotkey::effective(&chosen).to_owned();
    let hint = format!("Open or hide the popover from any app with {}.", hotkey::label(&current));
    let editable = !settings.read_only();
    let toggle = ui.add_enabled(editable, ToggleRow::new(&mut on, "Global shortcut", &hint));
    let mut picked = current.clone();
    if on {
        parts::card(ui, |ui| {
            let p = palette(ui.ctx());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Shortcut").font(Type::Row.font()).color(p.text));
                ui.add_enabled_ui(editable, |ui| {
                    egui::ComboBox::from_id_salt("shortcut").selected_text(hotkey::label(&picked)).show_ui(ui, |ui| {
                        for preset in hotkey::PRESETS {
                            ui.selectable_value(&mut picked, preset.spec.to_owned(), hotkey::label(preset.spec));
                        }
                    });
                });
            });
            match &settings.shortcut_status {
                hotkey::Status::Unavailable(why) => parts::caption(ui, &format!("Not working: {why}.")),
                _ => {
                    if let Some(note) = hotkey::caveat() {
                        parts::caption(ui, note);
                    }
                }
            }
        });
    }
    let config = settings.service.config();
    let mut failed = None;
    if toggle.changed()
        && let Some(config) = config
    {
        failed = config.set_shortcut_enabled(on).err();
        effects.shortcut = true;
    }
    if picked != current
        && let Some(config) = config
    {
        failed = config.set_shortcut(&picked).err().or(failed);
        effects.shortcut = true;
    }
    if let Some(err) = failed {
        toast(ui.ctx(), err.to_string(), Status::Failing);
    }
}

fn environment(ui: &mut Ui, state: &ProductState, installing: bool, can_install: bool, effects: &mut Effects) {
    let env = &state.env;
    parts::label(ui, "This computer");
    parts::card(ui, |ui| {
        let p = palette(ui.ctx());
        let row = |ui: &mut Ui, name: &str, ok: bool, value: String| {
            ui.horizontal(|ui| {
                let status = if ok { Status::Ready } else { Status::Attention };
                ui.label(RichText::new(status.icon()).font(signoff_ui::fonts::icon(14.0)).color(status.color(&p)));
                ui.label(RichText::new(name).font(Type::Body.font()).color(p.text));
                ui.label(RichText::new(value).font(Type::Caption.font()).color(p.secondary));
            });
        };
        row(
            ui,
            "GitHub CLI",
            env.gh_installed,
            if env.gh_installed { env.gh_version.clone() } else { "not found".into() },
        );
        row(
            ui,
            "Signed in",
            env.authenticated,
            if env.authenticated { env.login.clone() } else { "run gh auth login".into() },
        );
        row(
            ui,
            "gh signoff",
            env.signoff_installed,
            if env.signoff_installed {
                env.signoff_version.clone()
            } else {
                "run gh extension install basecamp/gh-signoff".into()
            },
        );
        if env.gh_installed && env.authenticated && !env.signoff_installed && can_install {
            ui.add_space(space::S);
            let label = if installing { "Installing…" } else { "Install" };
            if ui.add(Button::new(label).kind(ButtonKind::Secondary).enabled(!installing)).clicked() {
                effects.install = true;
            }
        }
        if !state.config_path.as_os_str().is_empty() {
            ui.add_space(space::XS);
            parts::caption(ui, &format!("Config: {}", state.config_path.display()));
        }
    });
}
