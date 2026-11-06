//! The app between windows, and its window.
//!
//! [`Signoff`] lives for the whole run and survives the window closing (the
//! app then keeps running in the tray). [`Window`] holds it while a window
//! is open. fastframe-shell recreates the window when the tray asks for it.

use fastframe_shell::{Closed, Headless, Held, Lease, Resident};
use fastframe_tray::{Event, Tray};
use signoff_core::{Health, ProductState};

use crate::popover::{self, Glance};
use crate::worker::Refresher;
use crate::{tray, views};

/// The app's state between windows.
pub(crate) struct Signoff {
    tray: Option<Tray>,
    refresher: Refresher,
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
    closing: Closed,
    show: bool,
    quit: bool,
}

impl Signoff {
    pub(crate) fn new(tray: Option<Tray>, refresher: Refresher, demo: bool) -> Self {
        Self {
            tray,
            refresher,
            state: ProductState::default(),
            seen: 0,
            shown_health: None,
            demo,
            glance: Glance::default(),
            settings_open: false,
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

    /// Handles tray clicks and new state. `ctx` is the window's, if open.
    fn tick(&mut self, ctx: Option<&egui::Context>) {
        let events = self.tray.as_ref().map(Tray::events).unwrap_or_default();
        for event in events {
            match event {
                Event::Show | Event::Menu(tray::OPEN) => self.show = true,
                Event::Toggle => match ctx {
                    Some(ctx) => {
                        self.closing = Closed::Hide;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
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
        self.refresher.stop();
    }
}

/// The open window, holding the app.
pub(crate) struct Window(Held<Signoff>);

/// Makes the window for `lease` once eframe has a context.
pub(crate) fn window(cc: &eframe::CreationContext<'_>, lease: Lease<Signoff>) -> Box<dyn eframe::App> {
    signoff_ui::install(&cc.egui_ctx, signoff_ui::ThemeMode::System);
    let mut app = lease.take(&cc.egui_ctx);
    app.closing = Closed::Hide;
    if let Some(tray) = &mut app.tray {
        tray.attach();
    }
    Box::new(Window(app))
}

/// The popover-sized main window.
pub(crate) fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Signoff")
            .with_app_id("signoff")
            .with_inner_size([380.0, 560.0])
            .with_min_inner_size([320.0, 360.0]),
        ..Default::default()
    }
}

impl eframe::App for Window {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.0.tick(Some(ctx));
        // Decided here, not in `ui`, which eframe skips while minimized.
        self.0.closing = if self.0.quit || !self.0.keeps_running() { Closed::Quit } else { Closed::Hide };
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        popover::show(ui, &mut self.0);
        if self.0.settings_open {
            views::settings_window(ui.ctx(), &mut self.0);
        }
    }
}
