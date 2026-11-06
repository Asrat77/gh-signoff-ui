//! The settings window, still a placeholder until it is designed and
//! built in its own step. The popover lives in [`crate::popover`].

use egui::{RichText, ViewportBuilder, ViewportId};

use crate::app::Signoff;

/// The settings window, as its own native window.
pub(crate) fn settings_window(ctx: &egui::Context, app: &mut Signoff) {
    let id = ViewportId::from_hash_of("settings");
    let builder = ViewportBuilder::default().with_title("Signoff Settings").with_inner_size([520.0, 420.0]);
    ctx.show_viewport_immediate(id, builder, |ui, _class| {
        if ui.ctx().input(|i| i.viewport().close_requested()) {
            app.settings_open = false;
        }
        let palette = signoff_ui::palette(ui.ctx());
        egui::Frame::new().inner_margin(16.0).show(ui, |ui| {
            ui.heading("Settings");
            ui.add_space(8.0);
            if app.demo {
                ui.label(RichText::new("Settings are read-only in demo mode.").color(palette.secondary));
            } else {
                ui.label(format!("Config file: {}", app.state.config_path.display()));
            }
            ui.add_space(8.0);
            ui.strong("Repositories");
            if app.state.repositories.is_empty() {
                ui.label(RichText::new("None yet.").color(palette.secondary));
            }
            for repo in &app.state.repositories {
                ui.label(format!("{}  ({})", repo.name_with_owner, repo.path.display()));
            }
            ui.add_space(8.0);
            ui.label(
                RichText::new("Adding repositories and mapping commands come in a later step.")
                    .color(palette.secondary),
            );
        });
    });
}
