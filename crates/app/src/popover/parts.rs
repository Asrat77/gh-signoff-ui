//! Pieces both popover pages share.

use egui::{Align, Frame, Layout, Margin, Rect, RichText, Sense, Ui, Vec2, ViewportCommand};
use signoff_core::Environment;
use signoff_ui::theme::{radius, size, space};
use signoff_ui::widgets::{Band, Button, ButtonKind, IconButton, Mood, spinner};
use signoff_ui::{Status, Type, icons, palette};

use crate::app::Signoff;

/// The ember band across the top. The window has no title bar, so the
/// band is also where it is dragged from. `trailing` adds the band's
/// buttons right to left.
pub(super) fn band(ui: &mut Ui, mood: Mood, demo: bool, trailing: impl FnOnce(&mut Ui)) {
    let band = if demo { Band::new(mood).subtitle("demo") } else { Band::new(mood) };
    // Registered before the band draws its buttons, so they sit on top of
    // it and still get their clicks.
    let top = ui.available_rect_before_wrap().min;
    let area = Rect::from_min_size(top, Vec2::new(ui.available_width(), size::BAND));
    let drag = ui.interact(area, ui.id().with("band-drag"), Sense::drag());
    let mut buttons = Rect::NOTHING;
    band.show(ui, |ui| {
        trailing(ui);
        buttons = ui.min_rect();
    });
    let from_button = ui.input(|i| i.pointer.press_origin()).is_some_and(|at| buttons.contains(at));
    if drag.drag_started() && !from_button {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

/// The band's refresh button, or a spinner in its place while a refresh
/// runs.
pub(super) fn refresh_button(ui: &mut Ui, app: &Signoff) {
    if app.refreshing() {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(size::ICON_BUTTON), Sense::hover());
        ui.painter().circle_filled(rect.center(), size::ICON_BUTTON / 2.0, egui::Color32::from_black_alpha(46));
        spinner(ui, rect.center(), 7.0, palette(ui.ctx()).on_accent);
    } else if ui.add(IconButton::new(icons::REFRESH, "Refresh")).clicked() {
        app.recheck();
    }
}

/// The band's search button, which opens the command palette; its tooltip
/// names the shortcut.
pub(super) fn palette_button(ui: &mut Ui, app: &mut Signoff) {
    if ui.add(IconButton::new(icons::SEARCH, &super::command_palette::hint())).clicked() {
        super::command_palette::toggle(app);
    }
}

/// A raised row with a glyph and wrapped text, for errors and warnings
/// that are not a pull request of their own.
pub(super) fn notice(ui: &mut Ui, status: Status, text: &str) {
    let p = palette(ui.ctx());
    Frame::new().fill(p.surface).corner_radius(radius::ROW).inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = space::S;
            ui.label(RichText::new(status.icon()).font(signoff_ui::fonts::icon(15.0)).color(status.color(&p)));
            ui.add(egui::Label::new(RichText::new(text).font(Type::Caption.font()).color(p.text)).wrap());
        });
    });
    ui.add_space(space::S);
}

/// Whether the popover should offer to install `gh signoff`: `gh` is ready
/// and only the extension is missing.
pub(super) fn needs_extension(env: &Environment) -> bool {
    env.gh_installed && env.authenticated && !env.signoff_installed
}

/// The notice that offers to install `gh signoff`, with its button. True
/// when the button was pressed. `failure` is why the last try failed.
pub(super) fn install_notice(ui: &mut Ui, installing: bool, failure: Option<&str>) -> bool {
    let p = palette(ui.ctx());
    let mut clicked = false;
    Frame::new().fill(p.surface).corner_radius(radius::ROW).inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = space::S;
            let status = if failure.is_some() { Status::Failing } else { Status::Attention };
            ui.label(RichText::new(status.icon()).font(signoff_ui::fonts::icon(15.0)).color(status.color(&p)));
            let text = failure.unwrap_or(
                "gh signoff is not installed, so Signoff cannot write signoffs. It adds the gh signoff extension for you.",
            );
            ui.add(egui::Label::new(RichText::new(text).font(Type::Caption.font()).color(p.text)).wrap());
        });
        ui.add_space(space::S);
        let label = if installing { "Installing gh signoff…" } else { "Install gh signoff" };
        clicked = ui.add(Button::new(label).kind(ButtonKind::Secondary).enabled(!installing)).clicked();
    });
    ui.add_space(space::S);
    clicked
}

/// A quiet centred line, such as "Updated 12s ago".
pub(super) fn caption(ui: &mut Ui, text: &str) {
    let p = palette(ui.ctx());
    ui.with_layout(Layout::top_down(Align::Center), |ui| {
        ui.label(RichText::new(text).font(Type::Caption.font()).color(p.secondary));
    });
}

/// The rest of the sheet's height after keeping `reserve` points free
/// below.
pub(super) fn height_above(ui: &Ui, reserve: f32) -> f32 {
    (ui.available_height() - reserve).max(size::ROW)
}

/// What must be fixed before there is anything to show: a title, a line,
/// and the page to send the person to.
pub(super) struct SetupProblem {
    pub(super) title: &'static str,
    pub(super) detail: &'static str,
    pub(super) action: &'static str,
    pub(super) url: Option<&'static str>,
}

/// What stops Signoff reading GitHub, if anything.
pub(super) fn setup_problem(env: &Environment) -> Option<SetupProblem> {
    if !env.gh_installed {
        Some(SetupProblem {
            title: "Install the GitHub CLI",
            detail: "Signoff reads GitHub through gh, with your own sign-in.",
            action: "Get gh",
            url: Some("https://cli.github.com"),
        })
    } else if !env.authenticated {
        Some(SetupProblem {
            title: "Sign in to GitHub",
            detail: "Run `gh auth login` in a terminal, then refresh.",
            action: "Refresh",
            url: None,
        })
    } else {
        None
    }
}
