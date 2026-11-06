//! "Found on this computer": GitHub checkouts the scan in
//! [`signoff_core::discover`] turned up, each one click from tracked.
//!
//! The scan runs as a settings job, once when a list first needs it and
//! again after a repository is added or removed.

use std::path::{Path, PathBuf};

use egui::{Align, Layout, RichText, ScrollArea, Ui};
use signoff_core::discover::{self, Found};
use signoff_ui::theme::space;
use signoff_ui::widgets::{Button, ButtonKind};
use signoff_ui::{Type, icons, palette};

use super::{Effects, Settings, parts, repositories};

/// Where the scan stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Scan {
    #[default]
    Never,
    Running,
    Done,
}

/// What the suggestions keep between frames.
#[derive(Debug)]
pub(crate) struct Discovery {
    /// The folder the common code folders sit under; None scans nothing.
    home: Option<PathBuf>,
    found: Vec<Found>,
    scan: Scan,
    /// The tracked set changed since the scan started.
    stale: bool,
}

impl Default for Discovery {
    fn default() -> Self {
        Self { home: directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_owned()), ..Self::empty() }
    }
}

impl Discovery {
    const fn empty() -> Self {
        Self { home: None, found: Vec::new(), scan: Scan::Never, stale: false }
    }

    /// Scans under `home` instead of the real home folder (None: nowhere).
    #[cfg(test)]
    pub(crate) fn scan_under(home: Option<PathBuf>) -> Self {
        Self { home, ..Self::empty() }
    }

    /// Shows `found` as if a scan had just returned them.
    #[cfg(test)]
    pub(crate) fn with_found(found: Vec<Found>) -> Self {
        Self { found, scan: Scan::Done, ..Self::empty() }
    }

    /// The tracked set changed: the next draw scans again.
    pub(super) fn mark_stale(&mut self) {
        self.stale = true;
    }

    /// Drops the suggestion for a checkout that was just added.
    pub(super) fn added(&mut self, path: &Path) {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        self.found.retain(|f| f.path != canonical && f.path != path);
        self.stale = true;
    }
}

/// Starts a scan when none has run or the tracked set changed.
fn ensure_scan(settings: &mut Settings) {
    let d = &settings.discovery;
    let needed = d.scan == Scan::Never || d.stale;
    let Some(home) = d.home.clone().filter(|_| needed && d.scan != Scan::Running) else { return };
    settings.discovery.scan = Scan::Running;
    settings.discovery.stale = false;
    settings.jobs.start(move |service| {
        let tracked: Vec<PathBuf> = service
            .config()
            .map(|config| config.snapshot().repositories.into_iter().map(|repo| repo.path).collect())
            .unwrap_or_default();
        let found = discover::scan_home(&home, &tracked);
        Box::new(move |settings: &mut Settings, _: &egui::Context, _: &mut Effects| {
            settings.discovery.found = found;
            settings.discovery.scan = Scan::Done;
        })
    });
}

/// `/home/me/code/app` → `~/code/app`.
fn tidy(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// Draws the suggestions: nothing while there are none. `compact` caps the
/// list's height for the popover's small first-run screen.
pub(super) fn show(ui: &mut Ui, settings: &mut Settings, compact: bool) {
    if settings.read_only() {
        return;
    }
    ensure_scan(settings);
    if settings.discovery.found.is_empty() {
        if settings.discovery.scan == Scan::Running && settings.discovery.home.is_some() {
            parts::caption(ui, "Looking for checkouts on this computer…");
        }
        return;
    }
    let p = palette(ui.ctx());
    let busy = settings.repositories.is_adding();
    let home = settings.discovery.home.clone();
    let found = settings.discovery.found.clone();
    let mut add: Vec<PathBuf> = Vec::new();

    ui.horizontal(|ui| {
        ui.label(RichText::new("Found on this computer").font(Type::Row.font()).color(p.text));
        if found.len() > 1 {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = format!("Add all {}", found.len());
                if ui.add(Button::new(&label).kind(ButtonKind::Accent).compact().enabled(!busy)).clicked() {
                    add.extend(found.iter().map(|f| f.path.clone()));
                }
            });
        }
    });
    ui.add_space(space::S);
    let mut rows = |ui: &mut Ui| {
        for item in &found {
            parts::card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::REPOSITORY).font(signoff_ui::fonts::icon(18.0)).color(p.secondary));
                    let text_width = (ui.available_width() - 84.0).max(80.0);
                    ui.vertical(|ui| {
                        ui.set_width(text_width);
                        ui.label(RichText::new(&item.name_with_owner).font(Type::Row.font()).color(p.text));
                        let path = tidy(&item.path, home.as_deref());
                        ui.add(
                            egui::Label::new(RichText::new(path).font(Type::Mono.font()).color(p.secondary)).truncate(),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let button = Button::new("Add").kind(ButtonKind::Accent).compact().width(64.0).enabled(!busy);
                        if ui.add(button).clicked() {
                            add.push(item.path.clone());
                        }
                    });
                });
            });
        }
    };
    if compact {
        ScrollArea::vertical().max_height(260.0).auto_shrink([false, true]).show(ui, rows);
    } else {
        rows(ui);
    }
    if !add.is_empty() {
        repositories::start_adding(settings, add);
    }
}
