//! General: launch at login, how often to refresh, and what Signoff found
//! on this computer.

use egui::{RichText, Slider, Ui};
use signoff_core::ProductState;
use signoff_ui::theme::space;
use signoff_ui::widgets::{ToggleRow, toast};
use signoff_ui::{Status, Type, palette};

use super::{Effects, Settings, parts};
use crate::autostart;

/// The refresh interval's bounds, in seconds, as the config accepts them.
const REFRESH_RANGE: std::ops::RangeInclusive<u32> = 10..=3600;

pub(super) fn show(ui: &mut Ui, settings: &mut Settings, state: &ProductState, effects: &mut Effects) {
    parts::heading(ui, "General", "How Signoff starts and how often it reads GitHub.");

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

    refresh_interval(ui, settings, effects);
    environment(ui, state);
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

fn refresh_interval(ui: &mut Ui, settings: &Settings, effects: &mut Effects) {
    let config = settings.service.config();
    let mut seconds = config.map_or(30, |config| config.snapshot().refresh_seconds);
    parts::card(ui, |ui| {
        let p = palette(ui.ctx());
        ui.label(RichText::new("Refresh every").font(Type::Row.font()).color(p.text));
        ui.add_space(space::XS);
        let slider = Slider::new(&mut seconds, REFRESH_RANGE).logarithmic(true).suffix(" s");
        let response = ui.add_enabled(!settings.read_only(), slider);
        if response.changed()
            && let Some(config) = config
            && let Err(err) = config.set_refresh_seconds(seconds)
        {
            toast(ui.ctx(), err.to_string(), Status::Failing);
        }
        // Refreshing now starts the new interval at once; wait for the drag
        // to end so a drag is one refresh, not many.
        if response.drag_stopped() || (response.changed() && !response.dragged()) {
            effects.refresh = true;
        }
        parts::caption(
            ui,
            "Signoff also reads again when you open the popover, if the last read is over 10 seconds old, or when you press refresh.",
        );
    });
}

fn environment(ui: &mut Ui, state: &ProductState) {
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
        if !state.config_path.as_os_str().is_empty() {
            ui.add_space(space::XS);
            parts::caption(ui, &format!("Config: {}", state.config_path.display()));
        }
    });
}
