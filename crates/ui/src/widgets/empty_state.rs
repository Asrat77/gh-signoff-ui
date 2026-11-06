//! What a screen shows when there is nothing in it yet.

use egui::{Align, Layout, Response, Ui, Widget};

use super::{Button, Eyes, Mood};
use crate::theme::space;
use crate::{Type, palette};

/// The eyes, a title, one line, and at most one action.
pub struct EmptyState<'a> {
    mood: Mood,
    title: &'a str,
    detail: &'a str,
    action: Option<&'a str>,
    clicked: Option<&'a mut bool>,
}

impl<'a> EmptyState<'a> {
    /// An empty state with the eyes in `mood`.
    #[must_use]
    pub const fn new(mood: Mood, title: &'a str, detail: &'a str) -> Self {
        Self { mood, title, detail, action: None, clicked: None }
    }

    /// A button reading `label`; `clicked` is set when it is pressed.
    #[must_use]
    pub fn action(mut self, label: &'a str, clicked: &'a mut bool) -> Self {
        self.action = Some(label);
        self.clicked = Some(clicked);
        self
    }
}

impl Widget for EmptyState<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = palette(ui.ctx());
        ui.with_layout(Layout::top_down(Align::Center), |ui| {
            ui.add_space(space::XXL);
            ui.add(Eyes::new(self.mood).size(44.0));
            ui.add_space(space::L);
            ui.label(egui::RichText::new(self.title).font(Type::Title.font()).color(p.text));
            ui.add_space(space::XS);
            ui.add(
                egui::Label::new(egui::RichText::new(self.detail).font(Type::Body.font()).color(p.secondary)).wrap(),
            );
            if let (Some(label), Some(clicked)) = (self.action, self.clicked) {
                ui.add_space(space::L);
                *clicked = ui.add(Button::new(label).width(220.0)).clicked();
            }
            ui.add_space(space::XXL);
        })
        .response
    }
}
