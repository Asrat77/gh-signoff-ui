//! Command output, with ANSI colours, that follows the end as it streams.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, CornerRadius, Frame, Margin, Response, ScrollArea, Ui, Widget};

use crate::ansi::{self, Style};
use crate::theme::{radius, space};
use crate::{Type, palette};

/// One line of output and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogLine {
    /// Printed by the command, possibly with ANSI codes.
    Output(String),
    /// A step Signoff took, such as `$ gh signoff tests`.
    Step(String),
    /// A success Signoff reports, such as `exit 0 in 6.2s`.
    Success(String),
    /// A failure Signoff reports.
    Failure(String),
}

/// A dark panel of command output.
pub struct LogView<'a> {
    lines: &'a [LogLine],
    title: &'a str,
    phase: &'a str,
    status: crate::Status,
    max_height: f32,
}

impl<'a> LogView<'a> {
    /// Shows `lines` under a header reading `title`.
    #[must_use]
    pub const fn new(title: &'a str, lines: &'a [LogLine]) -> Self {
        Self { lines, title, phase: "", status: crate::Status::Neutral, max_height: 220.0 }
    }

    /// Where the run is ("streaming", "exit 0"), shown on the right, and
    /// its state, which colours the dot.
    #[must_use]
    pub const fn phase(mut self, phase: &'a str, status: crate::Status) -> Self {
        self.phase = phase;
        self.status = status;
        self
    }

    /// The tallest the output area grows before it scrolls.
    #[must_use]
    pub const fn max_height(mut self, max_height: f32) -> Self {
        self.max_height = max_height;
        self
    }
}

/// Builds one layout job for every line, carrying ANSI styles across lines.
fn layout(lines: &[LogLine], text: Color32) -> LayoutJob {
    let font = Type::Mono.font();
    let dim = Color32::from_rgb(0x8A, 0x8A, 0x92);
    let mut job = LayoutJob::default();
    let mut style = Style::default();
    for (i, entry) in lines.iter().enumerate() {
        if i > 0 {
            job.append("\n", 0.0, TextFormat::simple(font.clone(), text));
        }
        let (body, fixed) = match entry {
            LogLine::Output(body) => (body, None),
            LogLine::Step(body) => (body, Some(dim)),
            LogLine::Success(body) => (body, Some(ansi::to_color32(ansi::Color::Indexed(2)))),
            LogLine::Failure(body) => (body, Some(ansi::to_color32(ansi::Color::Indexed(1)))),
        };
        if let Some(color) = fixed {
            job.append(&ansi::strip(body), 0.0, TextFormat::simple(font.clone(), color));
            continue;
        }
        let (spans, end) = ansi::parse(body, style);
        style = end;
        for span in spans {
            let mut color = span.style.fg.map_or(text, ansi::to_color32);
            if span.style.dim {
                color = color.gamma_multiply(0.6);
            }
            job.append(&span.text, 0.0, TextFormat::simple(font.clone(), color));
        }
    }
    job
}

impl Widget for LogView<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let p = palette(ui.ctx());
        Frame::new()
            .fill(p.panel)
            .corner_radius(CornerRadius::same(radius::ROW))
            .inner_margin(Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let dot = match self.status {
                        crate::Status::Running | crate::Status::Attention => p.accent,
                        crate::Status::Ready => p.success,
                        crate::Status::Failing => p.danger,
                        crate::Status::Neutral => p.outline,
                    };
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(7.0, 7.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 3.5, dot);
                    let header = Color32::from_rgb(0x9C, 0x9C, 0xA3);
                    ui.label(egui::RichText::new(self.title).font(Type::Caption.font()).color(header));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(self.phase).font(Type::Caption.font()).color(header));
                    });
                });
                ui.add_space(space::XS);
                ScrollArea::vertical()
                    .max_height(self.max_height)
                    .stick_to_bottom(true)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let mut job = layout(self.lines, p.terminal_text);
                        job.wrap.max_width = ui.available_width();
                        ui.add(egui::Label::new(job).selectable(true));
                    });
            })
            .response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_codes_become_colours_not_text() {
        let lines = [LogLine::Step("$ cargo test".into()), LogLine::Output("test a ... \u{1b}[32mok\u{1b}[0m".into())];
        let job = layout(&lines, Color32::WHITE);
        assert_eq!(job.text, "$ cargo test\ntest a ... ok");
        let ok = job
            .sections
            .iter()
            .find(|section| &job.text[section.byte_range.start.0..section.byte_range.end.0] == "ok")
            .expect("an ok section");
        assert_eq!(ok.format.color, ansi::to_color32(ansi::Color::Indexed(2)));
    }

    #[test]
    fn a_colour_left_open_carries_to_the_next_line() {
        let lines = [LogLine::Output("\u{1b}[31merror:".into()), LogLine::Output("  details".into())];
        let job = layout(&lines, Color32::WHITE);
        let last = job.sections.last().expect("a section");
        assert_eq!(last.format.color, ansi::to_color32(ansi::Color::Indexed(1)));
    }
}
