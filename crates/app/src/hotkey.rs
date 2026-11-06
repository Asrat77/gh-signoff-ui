//! The global shortcut that opens or hides the popover from anywhere.
//!
//! Registration is the OS's: `RegisterHotKey` on Windows, Carbon on macOS and
//! `XGrabKey` on X11. All three want the thread that registers to also run
//! the event loop, so [`Hotkeys::new`] runs on the main thread, the one
//! fastframe's shell and winit drive. Wayland has no such API; there
//! registration can fail or succeed without ever firing, so Settings says so.

use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver};

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

/// A shortcut Settings offers. `spec` is what the config stores, in
/// `global-hotkey`'s syntax; `CmdOrCtrl` is Cmd on macOS and Ctrl elsewhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Preset {
    pub(crate) spec: &'static str,
}

/// The shortcuts Settings offers, the default first. Cmd/Ctrl+Shift+Space is
/// the default because no common app binds it and Space needs no letter
/// that a layout may move; the others are for when it clashes.
pub(crate) const PRESETS: [Preset; 3] = [
    Preset { spec: "CmdOrCtrl+Shift+Space" },
    Preset { spec: "Alt+Shift+KeyS" },
    Preset { spec: "CmdOrCtrl+Alt+KeyS" },
];

/// The shortcut used while the config names none, or a bad one.
pub(crate) const DEFAULT: &str = PRESETS[0].spec;

/// What `spec` is when the config's string is empty or does not parse.
pub(crate) fn effective(spec: &str) -> &str {
    let spec = spec.trim();
    if !spec.is_empty() && parse(spec).is_ok() { spec } else { DEFAULT }
}

/// Parses a shortcut in `global-hotkey`'s syntax.
pub(crate) fn parse(spec: &str) -> Result<HotKey, String> {
    spec.parse::<HotKey>().map_err(|err| err.to_string())
}

/// `spec` as a person reads it: `Cmd+Shift+Space`, `Option+Shift+S`.
pub(crate) fn label(spec: &str) -> String {
    let mac = cfg!(target_os = "macos");
    spec.split('+')
        .map(|token| {
            let key = token.trim();
            match key.to_ascii_uppercase().as_str() {
                "CMDORCTRL" | "CMDORCONTROL" | "COMMANDORCTRL" | "COMMANDORCONTROL" => {
                    (if mac { "Cmd" } else { "Ctrl" }).to_owned()
                }
                "COMMAND" | "CMD" | "SUPER" | "META" => "Cmd".to_owned(),
                "CONTROL" | "CTRL" => "Ctrl".to_owned(),
                "OPTION" | "ALT" => (if mac { "Option" } else { "Alt" }).to_owned(),
                "SHIFT" => "Shift".to_owned(),
                upper if upper.len() == 4 && upper.starts_with("KEY") => upper[3..].to_owned(),
                _ => key.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// A note for Settings where the shortcut may silently not work.
pub(crate) fn caveat() -> Option<&'static str> {
    #[cfg(target_os = "linux")]
    if std::env::var("XDG_SESSION_TYPE").is_ok_and(|kind| kind.eq_ignore_ascii_case("wayland")) {
        return Some("Wayland does not let apps listen for global shortcuts, so this may not work in this session.");
    }
    None
}

/// Whether the shortcut is working, for Settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Status {
    /// Not registered: switched off, or this run never asked.
    #[default]
    Off,
    /// Registered under this label.
    Active(String),
    /// The OS refused; this is why.
    Unavailable(String),
}

/// The registered shortcut and the presses waiting to be read.
pub(crate) struct Hotkeys {
    /// `None` where the OS has no way to listen.
    manager: Option<GlobalHotKeyManager>,
    registered: Option<HotKey>,
    presses: Receiver<u32>,
    status: Status,
}

impl Hotkeys {
    /// Starts listening for presses, waking the UI for each. Call it once,
    /// on the main thread, before the event loop runs. Never fails: where
    /// the OS has no global shortcuts, [`Hotkeys::apply`] reports it as a
    /// status.
    pub(crate) fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (sender, presses) = mpsc::channel();
        let sender = Mutex::new(sender);
        // One handler for the process, in place of polling the crate's own
        // channel: a press wakes the shell the moment it happens.
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.state() == HotKeyState::Pressed {
                if let Ok(sender) = sender.lock() {
                    let _ = sender.send(event.id());
                }
                wake();
            }
        }));
        let manager = match GlobalHotKeyManager::new() {
            Ok(manager) => Some(manager),
            Err(err) => {
                log::info!("no global shortcut on this system: {err}");
                None
            }
        };
        Self { manager, registered: None, presses, status: Status::Off }
    }

    /// Registers `spec` (or the default, if it is empty or invalid) when
    /// `enabled`, and unregisters whatever was registered before.
    pub(crate) fn apply(&mut self, enabled: bool, spec: &str) {
        if let (Some(manager), Some(old)) = (&self.manager, self.registered.take())
            && let Err(err) = manager.unregister(old)
        {
            log::warn!("could not release the old global shortcut: {err}");
        }
        if !enabled {
            self.status = Status::Off;
            return;
        }
        let spec = effective(spec);
        let Some(manager) = &self.manager else {
            self.status = Status::Unavailable("this system has no global shortcuts".to_owned());
            return;
        };
        let hotkey = match parse(spec) {
            Ok(hotkey) => hotkey,
            Err(err) => {
                self.status = Status::Unavailable(err);
                return;
            }
        };
        match manager.register(hotkey) {
            Ok(()) => {
                log::info!("global shortcut: {}", label(spec));
                self.registered = Some(hotkey);
                self.status = Status::Active(label(spec));
            }
            Err(err) => {
                log::warn!("global shortcut {} is unavailable: {err}", label(spec));
                self.status = Status::Unavailable(err.to_string());
            }
        }
    }

    /// Whether the registered shortcut was pressed since the last call.
    pub(crate) fn take_pressed(&self) -> bool {
        let id = self.registered.map(|hotkey| hotkey.id());
        let mut pressed = false;
        while let Ok(got) = self.presses.try_recv() {
            pressed |= Some(got) == id;
        }
        pressed
    }

    pub(crate) fn status(&self) -> &Status {
        &self.status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_parses_and_is_distinct() {
        let ids: Vec<u32> = PRESETS.iter().map(|p| parse(p.spec).expect("preset parses").id()).collect();
        for (i, id) in ids.iter().enumerate() {
            assert!(!ids[i + 1..].contains(id), "{} repeats", PRESETS[i].spec);
        }
    }

    #[test]
    fn an_empty_or_bad_shortcut_falls_back_to_the_default() {
        assert_eq!(effective(""), DEFAULT);
        assert_eq!(effective("  "), DEFAULT);
        assert_eq!(effective("Shift+Banana"), DEFAULT);
        assert_eq!(effective("Alt+Shift+KeyS"), "Alt+Shift+KeyS");
        assert_eq!(effective(" Alt+Shift+KeyS "), "Alt+Shift+KeyS", "spaces are trimmed");
    }

    #[test]
    fn a_shortcut_needs_exactly_one_key() {
        assert!(parse("Ctrl+Shift").is_err());
        assert!(parse("Ctrl+A+B").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn labels_are_readable() {
        let cmd = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };
        let alt = if cfg!(target_os = "macos") { "Option" } else { "Alt" };
        assert_eq!(label("CmdOrCtrl+Shift+Space"), format!("{cmd}+Shift+Space"));
        assert_eq!(label("Alt+Shift+KeyS"), format!("{alt}+Shift+S"));
    }
}
