//! Pieces the settings sections share.

use egui::{Frame, Margin, RichText, TextEdit, Ui};
use signoff_ui::theme::{radius, space};
use signoff_ui::{Status, Type, palette};

/// A section's title and the line under it.
pub(super) fn heading(ui: &mut Ui, title: &str, detail: &str) {
    let p = palette(ui.ctx());
    ui.label(RichText::new(title).font(Type::Heading.font()).color(p.text));
    ui.add_space(2.0);
    ui.add(egui::Label::new(RichText::new(detail).font(Type::Caption.font()).color(p.secondary)).wrap());
    ui.add_space(space::M);
}

/// A small label above a group of rows.
pub(super) fn label(ui: &mut Ui, text: &str) {
    let p = palette(ui.ctx());
    ui.add_space(space::S);
    ui.label(RichText::new(text).font(Type::Caption.font()).color(p.secondary));
    ui.add_space(space::XS);
}

/// A raised row with a glyph and wrapped text, for errors and hints.
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

/// A raised card holding `add_contents`.
pub(super) fn card<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let p = palette(ui.ctx());
    let inner = Frame::new()
        .fill(p.surface)
        .corner_radius(radius::ROW)
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner;
    ui.add_space(space::S);
    inner
}

/// A one-line text input in the type scale's `step`, with a placeholder.
pub(super) fn input(ui: &mut Ui, text: &mut String, hint: &str, step: Type, width: f32) -> egui::Response {
    ui.add(
        TextEdit::singleline(text)
            .hint_text(hint)
            .font(step.font())
            .desired_width(width)
            .margin(Margin::symmetric(10, 8)),
    )
}

/// A muted caption line.
pub(super) fn caption(ui: &mut Ui, text: &str) {
    let p = palette(ui.ctx());
    ui.add(egui::Label::new(RichText::new(text).font(Type::Caption.font()).color(p.secondary)).wrap());
}
