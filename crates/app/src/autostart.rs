//! Launch at login, the way each desktop expects it.
//!
//! - macOS: a LaunchAgent plist in `~/Library/LaunchAgents`.
//! - Linux and other freedesktop systems: an XDG autostart `.desktop` entry.
//! - Windows: a value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//!
//! Each starts Signoff with `--hidden`, so login puts it in the tray without
//! opening a window. Nothing needs admin rights.

use std::path::{Path, PathBuf};

/// The name the entry is registered under.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const LABEL: &str = "dev.signoff.Signoff";

/// Whether Signoff starts at login.
pub(crate) fn is_enabled() -> bool {
    imp::is_enabled()
}

/// Turns launch at login on or off for the running executable.
///
/// # Errors
///
/// A message for the user when the entry could not be written or removed.
pub(crate) fn set_enabled(on: bool) -> Result<(), String> {
    let result = if on {
        // An AppImage runs from a temporary mount; start the image itself.
        let exe = match std::env::var_os("APPIMAGE") {
            Some(image) if cfg!(target_os = "linux") => PathBuf::from(image),
            _ => std::env::current_exe().map_err(|err| format!("could not find Signoff's executable: {err}"))?,
        };
        imp::enable(&exe)
    } else {
        imp::disable()
    };
    result.map_err(|err| format!("could not change launch at login: {err}"))
}

/// The command line the entry runs: the executable, then `--hidden`.
fn command(exe: &Path) -> [String; 2] {
    [exe.display().to_string(), "--hidden".to_owned()]
}

/// Escapes text for an XML string.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn xml_escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A LaunchAgent that runs `exe --hidden` at login.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn launch_agent(exe: &Path) -> String {
    let args: String =
        command(exe).iter().map(|arg| format!("        <string>{}</string>\n", xml_escape(arg))).collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
{args}    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#
    )
}

/// An XDG autostart entry that runs `exe --hidden` at login.
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn desktop_entry(exe: &Path) -> String {
    // The Exec key quotes arguments with double quotes and escapes `"`, `` ` ``,
    // `$` and `\` inside them.
    let quote = |arg: &str| {
        let escaped: String = arg
            .chars()
            .flat_map(|c| if matches!(c, '"' | '`' | '$' | '\\') { vec!['\\', c] } else { vec![c] })
            .collect();
        format!("\"{escaped}\"")
    };
    let exec = command(exe).iter().map(|arg| quote(arg)).collect::<Vec<_>>().join(" ");
    format!(
        "[Desktop Entry]\nType=Application\nName=Signoff\nComment=See and sign off your pull requests from the tray\nExec={exec}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
    )
}

/// The file an entry lives in, on systems that use one.
#[cfg_attr(windows, allow(dead_code))]
fn entry_path() -> Option<PathBuf> {
    let base = directories::BaseDirs::new()?;
    if cfg!(target_os = "macos") {
        Some(base.home_dir().join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
    } else {
        Some(base.config_dir().join("autostart").join("signoff.desktop"))
    }
}

#[cfg(not(windows))]
mod imp {
    use std::io;
    use std::path::Path;

    use super::entry_path;

    fn path() -> io::Result<std::path::PathBuf> {
        entry_path().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home folder"))
    }

    pub(super) fn is_enabled() -> bool {
        entry_path().is_some_and(|path| path.is_file())
    }

    pub(super) fn enable(exe: &Path) -> io::Result<()> {
        let path = path()?;
        let contents = if cfg!(target_os = "macos") { super::launch_agent(exe) } else { super::desktop_entry(exe) };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)
    }

    pub(super) fn disable() -> io::Result<()> {
        match std::fs::remove_file(path()?) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::process::CommandExt as _;
    use std::path::Path;
    use std::process::{Command, Stdio};

    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "Signoff";
    /// Keeps `reg.exe` from flashing a console window.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn reg(args: &[&str]) -> io::Result<bool> {
        let status = Command::new("reg")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status()?;
        Ok(status.success())
    }

    pub(super) fn is_enabled() -> bool {
        reg(&["query", KEY, "/v", VALUE]).unwrap_or(false)
    }

    pub(super) fn enable(exe: &Path) -> io::Result<()> {
        let [exe, flag] = super::command(exe);
        let data = format!("\"{exe}\" {flag}");
        if reg(&["add", KEY, "/v", VALUE, "/t", "REG_SZ", "/d", &data, "/f"])? {
            Ok(())
        } else {
            Err(io::Error::other("reg add failed"))
        }
    }

    pub(super) fn disable() -> io::Result<()> {
        if !is_enabled() || reg(&["delete", KEY, "/v", VALUE, "/f"])? {
            Ok(())
        } else {
            Err(io::Error::other("reg delete failed"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launch_agent_starts_hidden_and_escapes_the_path() {
        let plist = launch_agent(Path::new("/Applications/A & B/Signoff.app/Contents/MacOS/signoff"));
        assert!(plist.contains("<string>/Applications/A &amp; B/Signoff.app/Contents/MacOS/signoff</string>"));
        assert!(plist.contains("<string>--hidden</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n    <true/>"));
    }

    #[test]
    fn the_desktop_entry_quotes_the_path() {
        let entry = desktop_entry(Path::new("/opt/my $apps/signoff"));
        assert!(entry.contains("Exec=\"/opt/my \\$apps/signoff\" \"--hidden\"\n"));
        assert!(entry.starts_with("[Desktop Entry]\n"));
    }
}
