//! The widget gallery: every widget in the design system, on demo data.
//!
//! Run it with `cargo run -p signoff-ui --example gallery`. Pass `--light`
//! to start in the light palette, and `--run` to start the demo run.

use eframe::egui::{self, CentralPanel, Frame, Margin, ScrollArea, Ui, Vec2};
use signoff_ui::theme::{self, space};
use signoff_ui::widgets::{
    AddButton, Band, Button, ButtonKind, CheckRow, Chip, EmptyState, Eyes, Headline, IconButton, IconButtonKind,
    LiveIndicator, LogLine, LogView, Mood, PrRow, Segment, Segments, ToggleRow, sheet, show_toasts, toast,
};
use signoff_ui::{Status, ThemeMode, icons, palette};

fn main() -> eframe::Result {
    let light = std::env::args().any(|arg| arg == "--light");
    let run = std::env::args().any(|arg| arg == "--run");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Signoff widget gallery")
            .with_inner_size([1460.0, 900.0]),
        ..Default::default()
    };
    eframe::run_native(
        "signoff-gallery",
        options,
        Box::new(move |cc| {
            let mode = if light { ThemeMode::Light } else { ThemeMode::Dark };
            signoff_ui::install(&cc.egui_ctx, mode);
            let mut gallery = Gallery::new(mode);
            if run {
                gallery.phase = Phase::Running { started: 0.0 };
            }
            Ok(Box::new(gallery))
        }),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    Yours,
    All,
    Failing,
    Ready,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    Running { started: f64 },
    Done,
}

struct Pr {
    status: Status,
    title: &'static str,
    repo: &'static str,
    number: u64,
    branch: &'static str,
    checks: &'static [Segment],
    yours: bool,
}

const D: Segment = Segment::Done;
const E: Segment = Segment::Empty;

const PRS: [Pr; 6] = [
    Pr {
        status: Status::Failing,
        title: "Stream command output without buffering",
        repo: "gh-signoff",
        number: 479,
        branch: "fix/stream-output",
        checks: &[D, Segment::Current(Status::Failing), E],
        yours: true,
    },
    Pr {
        status: Status::Attention,
        title: "Add label-based partial signoff",
        repo: "gh-signoff",
        number: 482,
        branch: "feature/partial-checks",
        checks: &[D, D, E],
        yours: true,
    },
    Pr {
        status: Status::Attention,
        title: "Retry declined payments once",
        repo: "storefront",
        number: 1207,
        branch: "retry-declines",
        checks: &[D, E],
        yours: true,
    },
    Pr {
        status: Status::Running,
        title: "Cache branch protection per repo",
        repo: "gh-signoff",
        number: 485,
        branch: "perf/rules-cache",
        checks: &[D, Segment::Current(Status::Running), E],
        yours: false,
    },
    Pr {
        status: Status::Ready,
        title: "Upgrade image pipeline to AVIF",
        repo: "storefront",
        number: 1199,
        branch: "avif",
        checks: &[D, D],
        yours: true,
    },
    Pr {
        status: Status::Ready,
        title: "Remove legacy feature flags",
        repo: "storefront",
        number: 1188,
        branch: "chore/flags",
        checks: &[D, D, D],
        yours: false,
    },
];

const RUN: [&str; 8] = [
    "$ cargo audit",
    "    \u{1b}[1;32mFetching\u{1b}[0m advisory database",
    "      \u{1b}[1;32mLoaded\u{1b}[0m 712 security advisories",
    "    \u{1b}[1;32mScanning\u{1b}[0m Cargo.lock for vulnerabilities (214 deps)",
    "    \u{1b}[32mNo vulnerable packages found\u{1b}[0m",
    "exit 0 in 6.2s",
    "$ gh signoff security --commit 3db401c",
    "signed off security on 3db401c",
];

struct Gallery {
    mode: ThemeMode,
    filter: Filter,
    phase: Phase,
    settings: [bool; 3],
    reduced_motion: bool,
}

impl Gallery {
    fn new(mode: ThemeMode) -> Self {
        Self { mode, filter: Filter::Yours, phase: Phase::Idle, settings: [true, true, false], reduced_motion: false }
    }

    fn lines_shown(&self, now: f64) -> usize {
        match self.phase {
            Phase::Idle => 0,
            Phase::Done => RUN.len(),
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Phase::Running { started } => (((now - started) / 0.45) as usize + 1).min(RUN.len()),
        }
    }

    fn popover(ui: &mut Ui, height: f32, add: impl FnOnce(&mut Ui)) {
        let p = palette(ui.ctx());
        Frame::new().fill(p.accent).corner_radius(theme::radius::SHEET).show(ui, |ui| {
            ui.set_width(420.0);
            ui.set_height(height);
            ui.vertical(add);
        });
    }

    fn list(&mut self, ui: &mut Ui) {
        let mood = match self.filter {
            Filter::Failing => Mood::Worried,
            Filter::Ready => Mood::Relaxed,
            _ => Mood::AtYou,
        };
        Band::new(mood).show(ui, |ui| {
            ui.add(IconButton::new(icons::SETTINGS, "Settings"));
            if ui.add(IconButton::new(icons::REFRESH, "Refresh")).clicked() {
                toast(ui.ctx(), "Refreshed 2 repositories", Status::Ready);
            }
        });
        sheet(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_height(ui.available_height());
            Frame::new().inner_margin(Margin { left: 22, right: 22, top: 24, bottom: 16 }).show(ui, |ui| {
                let (title, detail) = if self.filter == Filter::Ready {
                    ("2 ready to merge", "Every required signoff passes on head.")
                } else {
                    ("3 PRs need you", "1 failing · 2 waiting on your signoff")
                };
                ui.add(Headline::new(title).detail(detail));
            });
            Frame::new().inner_margin(Margin::symmetric(18, 0)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (filter, label, count) in [
                        (Filter::Yours, "Yours", 4),
                        (Filter::All, "All", 6),
                        (Filter::Failing, "Failing", 1),
                        (Filter::Ready, "Ready", 2),
                    ] {
                        let chip = Chip::new(label).count(count).selected(self.filter == filter);
                        if ui.add(chip).clicked() {
                            self.filter = filter;
                        }
                    }
                });
            });
            ui.add_space(space::M + 2.0);
            Frame::new().inner_margin(Margin::symmetric(14, 0)).show(ui, |ui| {
                ScrollArea::vertical().max_height(330.0).show(ui, |ui| {
                    for pr in PRS.iter().filter(|pr| match self.filter {
                        Filter::Yours => pr.yours,
                        Filter::All => true,
                        Filter::Failing => pr.status == Status::Failing,
                        Filter::Ready => pr.status == Status::Ready,
                    }) {
                        ui.add(PrRow::new(pr.status, pr.title, pr.repo, pr.number).branch(pr.branch).checks(pr.checks));
                    }
                });
                ui.add_space(space::M);
                ui.add(Button::new("Fix #479 first").trailing_icon(icons::FORWARD));
                ui.add_space(space::S);
                ui.add(LiveIndicator::new(Status::Ready, "Live"));
            });
        });
    }

    fn detail(&mut self, ui: &mut Ui) {
        let now = ui.input(|i| i.time);
        let shown = self.lines_shown(now);
        if let Phase::Running { .. } = self.phase {
            if shown == RUN.len() {
                self.phase = Phase::Done;
                toast(ui.ctx(), "Signed off security on 3db401c", Status::Ready);
            } else {
                ui.ctx().request_repaint_after_secs(0.1);
            }
        }
        let (security, mood, headline_status) = match self.phase {
            Phase::Idle => (Status::Attention, Mood::AtYou, "2 of 3 signed off · security needs you"),
            Phase::Running { .. } => (Status::Running, Mood::Busy, "Running security on 3db401c…"),
            Phase::Done => (Status::Ready, Mood::Relaxed, "All 3 signoffs pass · ready to merge"),
        };
        Band::new(mood).show(ui, |ui| {
            ui.add(IconButton::new(icons::EXTERNAL, "Open on GitHub"));
        });
        sheet(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_height(ui.available_height());
            Frame::new().inner_margin(Margin { left: 18, right: 18, top: 18, bottom: 14 }).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            IconButton::new(icons::BACK, "Back to pull requests")
                                .kind(IconButtonKind::OnSheet)
                                .size(36.0),
                        )
                        .clicked()
                    {
                        self.phase = Phase::Idle;
                    }
                    ui.add_space(space::XS);
                    let segments = match self.phase {
                        Phase::Idle => [D, D, Segment::Current(Status::Attention)],
                        Phase::Running { .. } => [D, D, Segment::Current(Status::Running)],
                        Phase::Done => [D, D, D],
                    };
                    ui.add(Segments::new(&segments));
                });
                ui.add_space(space::L + 4.0);
                ui.add(
                    Headline::new("Add label-based partial signoff").detail(headline_status).title_size().centered(),
                );
            });
            Frame::new().inner_margin(Margin::symmetric(14, 0)).show(ui, |ui| {
                CheckRow::new("Tests", "cargo test --workspace", Status::Ready).category("tests").show(ui);
                CheckRow::new("Lint", "cargo clippy -- -D warnings", Status::Ready).category("lint").show(ui);
                CheckRow::new("Security", "cargo audit", security).category("security").show(ui);
                ui.add_space(space::XS);
                let mut lines: Vec<LogLine> = RUN[..shown]
                    .iter()
                    .map(|line| {
                        if line.starts_with('$') {
                            LogLine::Step((*line).to_owned())
                        } else if line.starts_with("exit") || line.starts_with("signed") {
                            LogLine::Success(format!("{} {line}", icons::CHECK))
                        } else {
                            LogLine::Output((*line).to_owned())
                        }
                    })
                    .collect();
                if self.phase == Phase::Idle {
                    lines = vec![
                        LogLine::Failure("Last run 22m ago failed: 3 vulnerabilities.".into()),
                        LogLine::Step("Run it again on the current head.".into()),
                    ];
                }
                let (phase, status) = match self.phase {
                    Phase::Idle => ("idle", Status::Neutral),
                    Phase::Running { .. } => ("streaming", Status::Running),
                    Phase::Done => ("exit 0", Status::Ready),
                };
                ui.add(LogView::new("security output", &lines).phase(phase, status).max_height(150.0));
                ui.add_space(space::M);
                match self.phase {
                    Phase::Done => {
                        ui.add(
                            Button::new("Open pull request to merge")
                                .kind(ButtonKind::Accent)
                                .trailing_icon(icons::FORWARD),
                        );
                    }
                    Phase::Running { .. } => {
                        ui.add(Button::new("Running…").icon(icons::RUN).enabled(false));
                    }
                    Phase::Idle => {
                        if ui.add(Button::new("Run & sign off security").icon(icons::RUN)).clicked() {
                            self.phase = Phase::Running { started: now };
                        }
                    }
                }
            });
        });
    }

    fn kit(&mut self, ui: &mut Ui) {
        let p = palette(ui.ctx());
        ui.add(Headline::new("Widget gallery").detail("Every piece the popover and settings use."));
        ui.add_space(space::L);
        ui.horizontal(|ui| {
            for (mode, label) in [(ThemeMode::Dark, "Dark"), (ThemeMode::Light, "Light")] {
                if ui.add(Chip::new(label).selected(self.mode == mode)).clicked() {
                    self.mode = mode;
                    ui.ctx().set_theme(mode);
                }
            }
            let motion = if self.reduced_motion { "Motion off" } else { "Motion on" };
            if ui.add(Chip::new(motion).selected(self.reduced_motion)).clicked() {
                self.reduced_motion = !self.reduced_motion;
                theme::set_reduced_motion(ui.ctx(), self.reduced_motion);
            }
        });
        ui.add_space(space::L);
        ui.horizontal(|ui| {
            for mood in [Mood::Relaxed, Mood::AtYou, Mood::Worried, Mood::Busy] {
                Frame::new().fill(p.surface).corner_radius(theme::radius::ROW).inner_margin(Margin::same(14)).show(
                    ui,
                    |ui| {
                        ui.add(Eyes::new(mood).size(30.0));
                    },
                );
            }
        });
        ui.add_space(space::L);
        ui.label(egui::RichText::new("Settings").font(signoff_ui::Type::Heading.font()).color(p.text));
        ui.add_space(space::S);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width(200.0);
                for (i, (icon, label)) in [
                    (icons::GENERAL, "General"),
                    (icons::REPOSITORY, "Repositories"),
                    (icons::TERMINAL, "Commands"),
                    (icons::BRANCH, "Branch rules"),
                    (icons::HISTORY, "History"),
                ]
                .into_iter()
                .enumerate()
                {
                    ui.add(Chip::new(label).icon(icon).width(190.0).selected(i == 0));
                }
            });
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                let [login, notify, count] = &mut self.settings;
                ui.add(ToggleRow::new(login, "Launch at login", "Start Signoff when you sign in."));
                ui.add(ToggleRow::new(
                    notify,
                    "Tell me when a PR is ready",
                    "A notification when every signoff passes.",
                ));
                ui.add(ToggleRow::new(count, "Show count in the menu bar", "How many PRs need you, next to the eyes."));
                if ui.add(AddButton::new("Add a local checkout")).clicked() {
                    toast(ui.ctx(), "Choose a folder to track", Status::Neutral);
                }
                ui.horizontal(|ui| {
                    ui.add(Button::new("Secondary").kind(ButtonKind::Secondary).width(160.0));
                    ui.add(Button::new("Add").kind(ButtonKind::Accent).compact().width(90.0));
                });
            });
        });
        ui.add_space(space::L);
        let mut clicked = false;
        Frame::new().fill(p.surface).corner_radius(theme::radius::ROW).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                EmptyState::new(
                    Mood::Relaxed,
                    "Everything is clear",
                    "No tracked pull request is waiting on a signoff.",
                )
                .action("Open on GitHub", &mut clicked),
            );
        });
    }
}

impl eframe::App for Gallery {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        CentralPanel::default().frame(Frame::new().fill(palette(&ctx).window).inner_margin(28)).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(28.0, space::S);
                Self::popover(ui, 760.0, |ui| self.list(ui));
                Self::popover(ui, 840.0, |ui| self.detail(ui));
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::splat(space::S);
                    ScrollArea::vertical().show(ui, |ui| self.kit(ui));
                });
            });
        });
        show_toasts(&ctx);
    }
}
