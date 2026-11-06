//! Signoff: your pull requests' signoff checks, from the tray.
//!
//! The app is a tray item and a small window. Closing the window keeps the
//! app in the tray; the tray menu brings it back or quits. All GitHub reads
//! happen on a background thread (see [`worker`]), so the window never
//! waits on `gh`.

mod anchor;
mod app;
mod autostart;
mod checkouts;
mod commands;
mod glance;
mod hotkey;
mod installer;
mod notifier;
mod paths;
mod popover;
mod runs;
mod settings;
mod tray;
#[cfg(test)]
mod ui_tests;
mod worker;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use fastframe_shell::{Shell, Waker};
use signoff_core::api::UreqTransport;
use signoff_core::{ConfigStore, Gh, HistoryStore, ProductService, Runner, SystemRunner, Verifier};

/// What the command line asked for.
#[derive(Debug, Default)]
struct Args {
    demo: bool,
    hidden: bool,
    gh_only: bool,
    config: Option<PathBuf>,
}

const USAGE: &str = "\
Usage: signoff [options]

Options:
  --demo           Show fixed sample data; nothing is read from or written to GitHub
  --hidden         Start in the tray without opening the window
  --gh-only        Read GitHub through gh only, not over its API in-process
  --config <path>  Use this config file instead of the default
  --version        Print the version and exit
  --help           Print this help and exit";

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args { demo: cfg!(feature = "demo"), ..Args::default() };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--demo" => args.demo = true,
            "--hidden" => args.hidden = true,
            "--gh-only" => args.gh_only = true,
            "--config" => args.config = Some(argv.next().ok_or("--config needs a path")?.into()),
            "--version" | "-V" => {
                println!("Signoff {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(None);
            }
            other => return Err(format!("unknown option: {other}\n\n{USAGE}")),
        }
    }
    Ok(Some(args))
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("signoff=info,signoff_core=info"))
        .init();

    let args = match parse_args() {
        Ok(Some(args)) => args,
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    let (service, verifier, gh) = match build_service(&args) {
        Ok(built) => built,
        Err(message) => {
            log::error!("{message}");
            eprintln!("Signoff could not start: {message}");
            return ExitCode::FAILURE;
        }
    };

    let waker = Waker::default();
    let settings = {
        let wake = waker.clone();
        settings::Settings::new(service.clone(), move || wake.wake())
    };
    let refresher = {
        let wake = waker.clone();
        worker::Refresher::spawn(service, move || wake.wake())
    };
    let runs = {
        let wake = waker.clone();
        runs::Runs::new(verifier, move || wake.wake())
    };
    let checkouts = {
        let wake = waker.clone();
        checkouts::Checkouts::new(gh.clone(), args.demo, move || wake.wake())
    };
    let installer = {
        let wake = waker.clone();
        installer::Installer::new(gh, args.demo, move || wake.wake())
    };
    let tray = {
        let wake = waker.clone();
        tray::spawn(move || wake.wake())
    };
    if tray.is_none() {
        log::warn!("no tray is available; closing the window will quit");
    }

    // On this, the main thread: the OS delivers shortcut presses to the
    // thread that registered them, and this is the one that runs the loop.
    let hotkeys = {
        let wake = waker.clone();
        hotkey::Hotkeys::new(move || wake.wake())
    };
    let app = app::Signoff::new(tray, refresher, runs, checkouts, installer, settings, args.demo).with_hotkeys(hotkeys);
    let result = Shell::new(app, &waker).idle(fastframe_tray::idle).start_hidden(args.hidden).run(|lease| {
        let options = lease.peek(|app| app::native_options(app.is_popover(), app.anchor()));
        eframe::run_native("Signoff", options, Box::new(move |cc| Ok(app::window(cc, lease))))
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            log::error!("window failed: {err}");
            ExitCode::FAILURE
        }
    }
}

/// The state service, the verifier that runs checks against it and the
/// `gh` client for checking pull requests out.
fn build_service(args: &Args) -> Result<(Arc<ProductService>, Arc<Verifier>, Gh), String> {
    if args.demo {
        log::info!("demo mode: nothing is read from or written to GitHub");
        let service = Arc::new(ProductService::demo());
        let verifier = Arc::new(Verifier::demo(service.clone()));
        return Ok((service, verifier, Gh::default()));
    }
    let config_path = match &args.config {
        Some(path) => path.clone(),
        None => paths::config_file()?,
    };
    log::info!("config: {}", config_path.display());
    let config = ConfigStore::open(&config_path).map_err(|err| err.to_string())?;
    let history =
        HistoryStore::open(signoff_core::history::default_history_path(&config_path)).map_err(|err| err.to_string())?;
    let (config, history) = (Arc::new(config), Arc::new(history));
    let runner: Arc<dyn Runner> = Arc::new(SystemRunner::default());
    let seed_dir = std::env::current_dir().ok();
    // Reads go over GitHub's API with gh's token, falling back to gh by
    // themselves; --gh-only is the way out if the API path misbehaves.
    let gh = if args.gh_only {
        log::info!("reading GitHub through gh only");
        Gh::new(runner.clone())
    } else {
        Gh::with_api(runner.clone(), Arc::new(UreqTransport::default()))
    };
    let service = Arc::new(ProductService::new(gh.clone(), config.clone(), history.clone(), seed_dir));
    let verifier = Arc::new(Verifier::new(service.clone(), runner, config, history));
    Ok((service, verifier, gh))
}
