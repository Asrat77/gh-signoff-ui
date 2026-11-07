//! The command palette: a sheet over the popover with a search field and
//! the best few actions. Cmd+K (Ctrl+K elsewhere) opens it; arrows move,
//! Enter runs, Escape or a click outside closes it.

use egui::{
    Area, Context, CornerRadius, Frame, Id, Key, Margin, Modifiers, Order, Rect, RichText, Sense, TextEdit, Ui,
    UiBuilder, Vec2,
};
use signoff_core::VerifyRequest;
use signoff_ui::theme::{radius, space};
use signoff_ui::widgets::toast;
use signoff_ui::{Status, Type, palette};

use super::Page;
use crate::app::Signoff;
use crate::commands::{self, Action, Command};

/// Results shown at once.
const VISIBLE: usize = 8;
/// A result's height.
const ROW: f32 = 48.0;

/// What an open palette keeps between frames.
#[derive(Debug, Default)]
pub(crate) struct CommandPalette {
    query: String,
    selected: usize,
}

impl CommandPalette {
    /// The highlighted result's position.
    #[cfg(test)]
    pub(crate) const fn selected(&self) -> usize {
        self.selected
    }
}

/// The shortcut as people read it on this platform.
pub(crate) fn shortcut() -> &'static str {
    if cfg!(target_os = "macos") { "Cmd K" } else { "Ctrl K" }
}

/// The tooltip of the button that opens the palette.
pub(super) fn hint() -> String {
    format!("Search and run actions ({})", shortcut())
}

/// Opens the palette, or closes it when it is open.
pub(crate) fn toggle(app: &mut Signoff) {
    app.glance.palette = match app.glance.palette {
        Some(_) => None,
        None => Some(CommandPalette::default()),
    };
}

/// Handles the shortcut and draws the palette if it is open. Call after
/// the page is drawn so it lies over it.
pub(super) fn show(ui: &mut Ui, app: &mut Signoff) {
    let ctx = ui.ctx().clone();
    if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::K)) {
        toggle(app);
    }
    let Some(mut state) = app.glance.palette.take() else { return };
    let isolated = !app.demo && app.settings.isolated_worktrees_on();
    let actions = commands::actions(
        &app.state,
        |pr| pr.checked_out || app.checkouts.simulated(&pr.repo_id, pr.pr.number),
        isolated,
    );

    // Taken before the field sees them.
    let (escape, down, up, enter) = ctx.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::Escape),
            i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::ArrowUp),
            i.consume_key(Modifiers::NONE, Key::Enter),
        )
    });
    if escape {
        return;
    }
    let before = state.query.clone();
    let mut ranked = commands::rank(&actions, &state.query);
    ranked.truncate(VISIBLE);
    if down {
        state.selected = (state.selected + 1).min(ranked.len().saturating_sub(1));
    }
    if up {
        state.selected = state.selected.saturating_sub(1);
    }
    let mut run = if enter { ranked.get(state.selected).copied() } else { None };
    let mut close = false;
    let content = ctx.content_rect();
    Area::new(Id::new("command-palette")).order(Order::Foreground).fixed_pos(content.min).show(&ctx, |ui| {
        let (rect, scrim) = ui.allocate_exact_size(content.size(), Sense::click());
        let p = palette(&ctx);
        let corners = if app.is_popover() { radius::SHEET } else { 0 };
        ui.painter().rect_filled(rect, CornerRadius::same(corners), p.overlay);
        let card = Rect::from_min_size(
            rect.min + Vec2::new(space::M + 2.0, space::XXL * 2.0),
            Vec2::new(rect.width() - 2.0 * (space::M + 2.0), 0.0),
        );
        let drawn = ui.scope_builder(UiBuilder::new().max_rect(card), |ui| {
            Frame::new()
                .fill(p.window)
                .stroke(egui::Stroke::new(1.0, p.outline))
                .corner_radius(radius::ROW)
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = space::XS;
                    search(ui, &mut state.query);
                    ui.add_space(space::S);
                    if ranked.is_empty() {
                        no_match(ui);
                    }
                    for (slot, index) in ranked.iter().enumerate() {
                        if result(ui, &actions[*index], slot == state.selected).clicked() {
                            run = Some(*index);
                        }
                    }
                })
                .response
                .rect
        });
        close = scrim.clicked() && ctx.pointer_interact_pos().is_some_and(|at| !drawn.inner.contains(at));
    });
    if state.query != before {
        state.selected = 0;
    }
    if let Some(index) = run {
        execute(app, &ctx, &actions[index].command);
    } else if !close {
        app.glance.palette = Some(state);
    }
}

/// The mono search field, focused every frame it is open.
fn search(ui: &mut Ui, query: &mut String) {
    let hint = format!("Search pull requests and actions ({})", shortcut());
    let field = TextEdit::singleline(query)
        .hint_text(hint)
        .font(Type::Mono.font())
        .desired_width(f32::INFINITY)
        .margin(Margin::symmetric(10, 10));
    ui.add(field).request_focus();
}

fn no_match(ui: &mut Ui) {
    let p = palette(ui.ctx());
    ui.add_space(space::S);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("No matching action").font(Type::Caption.font()).color(p.secondary));
    });
    ui.add_space(space::S);
}

/// One result; the selected one is filled with the ember accent.
fn result(ui: &mut Ui, action: &Action, selected: bool) -> egui::Response {
    let p = palette(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{}, {}", action.label, action.meta))
    });
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let (fill, title, meta) = if selected {
        (p.accent, p.on_accent, p.on_accent.gamma_multiply(0.8))
    } else if response.hovered() {
        (p.surface_hover, p.text, p.secondary)
    } else {
        (p.surface, p.text, p.secondary)
    };
    ui.painter().rect_filled(rect, CornerRadius::same(radius::INPUT), fill);
    let left = rect.left() + space::M;
    let width = rect.width() - 2.0 * space::M;
    let clip = ui.painter().with_clip_rect(rect.shrink2(Vec2::new(space::S, 0.0)));
    let step = if action.label.starts_with("Copy ") { Type::Mono } else { Type::Row };
    for (text, step, color, top) in [(&action.label, step, title, 8.0), (&action.meta, Type::Caption, meta, 28.0)] {
        let galley = ui.painter().layout(text.clone(), step.font(), color, width);
        clip.galley(egui::pos2(left, rect.top() + top), galley, color);
    }
    response
}

/// Does what the chosen action says.
fn execute(app: &mut Signoff, ctx: &Context, command: &Command) {
    match command {
        Command::OpenPage { repo_id, number } => open_page(app, repo_id, *number, None),
        Command::Run { repo_id, number, check } => {
            open_page(app, repo_id, *number, Some(check));
            app.runs.start(VerifyRequest { repo_id: repo_id.clone(), pr_number: *number, check: check.clone() });
        }
        Command::CheckOut { repo_id, number, path } => {
            open_page(app, repo_id, *number, None);
            if !app.demo && app.settings.isolated_worktrees_on() {
                // Not the main action: the page asks before switching the
                // checkout's branch.
                app.glance.confirm_checkout = Some((repo_id.clone(), *number));
            } else {
                app.checkouts.start(repo_id, *number, path.clone());
            }
        }
        Command::Copy(text) => {
            ctx.copy_text(text.clone());
            toast(ctx, "Command copied", Status::Ready);
        }
        Command::OpenUrl(url) => crate::browser::open(ctx, url),
        Command::Refresh => app.recheck(),
        Command::Settings(section) => {
            app.settings.section = *section;
            app.settings_open = true;
        }
    }
}

fn open_page(app: &mut Signoff, repo_id: &str, number: u64, check: Option<&String>) {
    app.glance.page = Page::PullRequest { repo_id: repo_id.to_owned(), number };
    app.glance.check = check.cloned();
}
