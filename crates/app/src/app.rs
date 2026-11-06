//! The app between windows, and its window.
//!
//! [`Signoff`] lives for the whole run and survives the window closing (the
//! app then keeps running in the tray). [`Window`] holds it while a window
//! is open. fastframe-shell recreates the window when the tray asks for it.

use std::time::{Duration, Instant};

use egui::{Pos2, ViewportCommand};
use fastframe_shell::{Closed, Headless, Held, Lease, Resident};
use fastframe_tray::{Event, Tray};
use signoff_core::{Health, ProductState};

use crate::anchor::{self, Corner, POPOVER};
use crate::popover::{self, Glance};
use crate::runs::Runs;
use crate::worker::Refresher;
use crate::{tray, views};

/// The app's state between windows.
pub(crate) struct Signoff {
    tray: Option<Tray>,
    refresher: Refresher,
    /// Verify-and-sign-off runs.
    pub(crate) runs: Runs,
    /// The latest state copied from the refresher.
    pub(crate) state: ProductState,
    seen: u64,
    /// The health the tray icon shows.
    shown_health: Option<Health>,
    pub(crate) demo: bool,
    /// What the popover shows.
    pub(crate) glance: Glance,
    /// The settings window is open.
    pub(crate) settings_open: bool,
    /// Where the popover last opened, so the next window opens there.
    anchor: Option<Pos2>,
    /// When the popover last hid itself for losing focus.
    auto_hidden_at: Option<Instant>,
    closing: Closed,
    show: bool,
    quit: bool,
}

/// How long after the popover hides on losing focus a tray click is taken
/// as the same click, not a new one. Clicking the tray icon while the
/// popover is open takes focus from it first; without this the click would
/// hide it and open it again at once.
const SAME_CLICK: Duration = Duration::from_millis(400);

impl Signoff {
    pub(crate) fn new(tray: Option<Tray>, refresher: Refresher, runs: Runs, demo: bool) -> Self {
        Self {
            tray,
            refresher,
            runs,
            state: ProductState::default(),
            seen: 0,
            shown_health: None,
            demo,
            glance: Glance::default(),
            settings_open: false,
            anchor: None,
            auto_hidden_at: None,
            closing: Closed::Hide,
            show: false,
            quit: false,
        }
    }

    /// Whether a refresh is running.
    pub(crate) fn refreshing(&self) -> bool {
        self.refresher.is_refreshing()
    }

    /// Asks the background thread for a refresh.
    pub(crate) fn refresh(&self) {
        self.refresher.refresh();
    }

    /// Whether closing the window keeps the app running.
    fn keeps_running(&self) -> bool {
        self.tray.as_ref().is_some_and(Tray::is_shown)
    }

    /// Whether the window is the tray popover: borderless, anchored and
    /// hidden when it loses focus. Without a tray it is an ordinary window.
    pub(crate) fn is_popover(&self) -> bool {
        self.keeps_running()
    }

    /// Where the next popover window opens, once a window has measured the
    /// monitor.
    pub(crate) fn anchor(&self) -> Option<Pos2> {
        self.anchor
    }

    /// Whether a tray click now is the one that just hid the popover.
    fn just_hid(&self) -> bool {
        self.auto_hidden_at.is_some_and(|at| at.elapsed() < SAME_CLICK)
    }

    /// Hides the popover; the app stays in the tray.
    fn hide(&mut self, ctx: &egui::Context) {
        self.closing = Closed::Hide;
        self.auto_hidden_at = Some(Instant::now());
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    /// Handles tray clicks and new state. `ctx` is the window's, if open.
    fn tick(&mut self, ctx: Option<&egui::Context>) {
        let events = self.tray.as_ref().map(Tray::events).unwrap_or_default();
        for event in events {
            match event {
                Event::Show | Event::Toggle if ctx.is_none() && self.just_hid() => {}
                Event::Show | Event::Menu(tray::OPEN) => self.show = true,
                Event::Toggle => match ctx {
                    Some(ctx) => self.hide(ctx),
                    None => self.show = true,
                },
                Event::Menu(tray::REFRESH) => self.refresh(),
                Event::Menu(tray::SETTINGS) => {
                    self.settings_open = true;
                    self.show = true;
                }
                Event::Menu(tray::QUIT) => {
                    self.quit = true;
                    if let Some(ctx) = ctx {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                Event::Menu(_) => {}
            }
        }

        // A signoff changes GitHub's statuses; show them.
        if self.runs.take_signed_off() {
            self.refresh();
        }
        if let Some(state) = self.refresher.changed_since(&mut self.seen) {
            self.state = state;
        }
        let health = self.state.summary.status;
        if self.shown_health != Some(health)
            && let Some(tray) = &mut self.tray
        {
            tray.set_icon(tray::icon(health), None);
            tray.set_tooltip(tray::tooltip(&self.state.summary, self.demo));
            self.shown_health = Some(health);
        }

        if self.show
            && let Some(ctx) = ctx
        {
            self.show = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }
}

impl Resident for Signoff {
    fn closed(&self) -> Closed {
        self.closing
    }

    fn window_gone(&mut self) {
        self.show = false;
        self.settings_open = false;
    }

    fn start_hidden(&mut self) -> bool {
        match &mut self.tray {
            Some(tray) => {
                tray.create_item();
                true
            }
            None => false,
        }
    }

    fn headless_frame(&mut self, _ctx: &egui::Context) -> Headless {
        self.tick(None);
        if self.quit {
            Headless::Quit
        } else if self.show {
            Headless::Show
        } else {
            Headless::Wait
        }
    }

    fn shutdown(&mut self) {
        self.runs.stop_all();
        self.refresher.stop();
    }
}

/// The open window, holding the app.
pub(crate) struct Window {
    app: Held<Signoff>,
    /// The window has been moved to its corner.
    placed: bool,
    /// The window has had focus since it opened.
    focused: bool,
}

/// Makes the window for `lease` once eframe has a context.
pub(crate) fn window(cc: &eframe::CreationContext<'_>, lease: Lease<Signoff>) -> Box<dyn eframe::App> {
    signoff_ui::install(&cc.egui_ctx, signoff_ui::ThemeMode::System);
    let mut app = lease.take(&cc.egui_ctx);
    app.closing = Closed::Hide;
    if let Some(tray) = &mut app.tray {
        tray.attach();
    }
    Box::new(Window { app, placed: false, focused: false })
}

/// The main window: with a tray, a borderless popover that floats over
/// other windows at `anchor`; without one, an ordinary window.
pub(crate) fn native_options(popover: bool, anchor: Option<Pos2>) -> eframe::NativeOptions {
    let mut viewport = egui::ViewportBuilder::default().with_title("Signoff").with_app_id("signoff");
    viewport = if popover {
        let viewport = viewport
            .with_inner_size(POPOVER)
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_always_on_top()
            .with_taskbar(false);
        match anchor {
            Some(at) => viewport.with_position(at),
            None => viewport,
        }
    } else {
        viewport.with_inner_size(POPOVER).with_min_inner_size([340.0, 420.0])
    };
    eframe::NativeOptions { viewport, ..Default::default() }
}

impl Window {
    /// Moves a new popover to the tray's corner once the monitor is known,
    /// unless it already opened where the last one was.
    fn place(&mut self, ctx: &egui::Context) {
        if self.placed || !self.app.is_popover() {
            return;
        }
        let Some(monitor) = ctx.input(|i| i.viewport().monitor_size) else { return };
        let at = anchor::anchor(monitor, POPOVER, Corner::of_tray());
        if self.app.anchor != Some(at) {
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(at));
            self.app.anchor = Some(at);
        }
        self.placed = true;
    }

    /// Hides the popover when it loses focus or Escape is pressed, as a
    /// menu-bar popover does. Settings opens its own window, which takes
    /// focus, so the popover stays while it is open.
    fn dismiss_when_done(&mut self, ctx: &egui::Context) {
        if !self.app.is_popover() || self.app.quit {
            return;
        }
        let (focused, escape) = ctx.input(|i| (i.viewport().focused, i.key_pressed(egui::Key::Escape)));
        match focused {
            Some(true) => self.focused = true,
            Some(false) if self.focused && !self.app.settings_open => {
                self.focused = false;
                self.app.hide(ctx);
            }
            _ => {}
        }
        if escape {
            self.app.hide(ctx);
        }
    }
}

impl eframe::App for Window {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.app.tick(Some(ctx));
        self.place(ctx);
        self.dismiss_when_done(ctx);
        // Decided here, not in `ui`, which eframe skips while minimized.
        self.app.closing = if self.app.quit || !self.app.keeps_running() { Closed::Quit } else { Closed::Hide };
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        popover::show(ui, &mut self.app);
        if self.app.settings_open {
            views::settings_window(ui.ctx(), &mut self.app);
        }
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        // The popover draws its own rounded card; around it is see-through.
        if self.app.is_popover() { [0.0; 4] } else { visuals.panel_fill.to_normalized_gamma_f32() }
    }
}
