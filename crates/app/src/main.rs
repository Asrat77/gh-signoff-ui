//! Signoff: your pull requests' signoff checks, from the tray.
//!
//! The app is a tray item and a small window. Closing the window keeps the
//! app in the tray; the tray menu brings it back or quits. All GitHub reads
//! happen on a background thread (see [`worker`]), so the window never
//! waits on `gh`.

mod app;
mod paths;
mod theme;
mod tray;
mod views;
mod worker;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use fastframe_shell::{Shell, Waker};
use signoff_core::{ConfigStore, Gh, HistoryStore, ProductService};

/// What the command line asked for.
#[derive(Debug, Default)]
struct Args {
    demo: bool,
    hidden: bool,
    config: Option<PathBuf>,
}

const USAGE: &str = "\
Usage: signoff [options]

Options:
  --demo           Show fixed sample data; nothing is read from or written to GitHub
  --hidden         Start in the tray without opening the window
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

    let service = match build_service(&args) {
        Ok(service) => Arc::new(service),
        Err(message) => {
            log::error!("{message}");
            eprintln!("Signoff could not start: {message}");
            return ExitCode::FAILURE;
        }
    };

    let waker = Waker::default();
    let refresher = {
        let wake = waker.clone();
        worker::Refresher::spawn(service, move || wake.wake())
    };
    let tray = {
        let wake = waker.clone();
        tray::spawn(move || wake.wake())
    };
    if tray.is_none() {
        log::warn!("no tray is available; closing the window will quit");
    }

    let app = app::Signoff::new(tray, refresher, args.demo);
    let result = Shell::new(app, &waker).idle(fastframe_tray::idle).start_hidden(args.hidden).run(|lease| {
        eframe::run_native("Signoff", app::native_options(), Box::new(move |cc| Ok(app::window(cc, lease))))
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            log::error!("window failed: {err}");
            ExitCode::FAILURE
        }
    }
}

fn build_service(args: &Args) -> Result<ProductService, String> {
    if args.demo {
        log::info!("demo mode: nothing is read from or written to GitHub");
        return Ok(ProductService::demo());
    }
    let config_path = match &args.config {
        Some(path) => path.clone(),
        None => paths::config_file()?,
    };
    log::info!("config: {}", config_path.display());
    let config = ConfigStore::open(&config_path).map_err(|err| err.to_string())?;
    let history =
        HistoryStore::open(signoff_core::history::default_history_path(&config_path)).map_err(|err| err.to_string())?;
    let seed_dir = std::env::current_dir().ok();
    Ok(ProductService::new(Gh::default(), Arc::new(config), Arc::new(history), seed_dir))
}
