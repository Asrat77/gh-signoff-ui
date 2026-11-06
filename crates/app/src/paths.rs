//! Where Signoff keeps its files.

use std::path::{Path, PathBuf};

/// The platform's per-user folders for Signoff.
const DIRS: fastframe_dirs::App = fastframe_dirs::App::new("signoff", "dev", "Signoff", "Signoff");

/// The config file, with `history.json` beside it.
///
/// The first time it does not exist, the files the Go version kept in
/// `<user config dir>/Signoff` are copied over, so tracked repositories and
/// history carry across the rewrite. They are copied, not moved, so the old
/// app keeps working too.
pub(crate) fn config_file() -> Result<PathBuf, String> {
    let dirs = DIRS.installed().ok_or("no home folder to keep the config in")?;
    let path = dirs.config.join("config.json");
    if !path.exists()
        && let Some(legacy) = directories::BaseDirs::new().map(|base| base.config_dir().join("Signoff"))
        && legacy != dirs.config
    {
        adopt_legacy(&legacy, &dirs.config);
    }
    Ok(path)
}

fn adopt_legacy(from: &Path, to: &Path) {
    if !from.join("config.json").is_file() {
        return;
    }
    if let Err(err) = std::fs::create_dir_all(to) {
        log::warn!("could not create {}: {err}", to.display());
        return;
    }
    for name in ["config.json", "history.json"] {
        let source = from.join(name);
        if source.is_file() {
            match std::fs::copy(&source, to.join(name)) {
                Ok(_) => log::info!("copied {} from the previous version", source.display()),
                Err(err) => log::warn!("could not copy {}: {err}", source.display()),
            }
        }
    }
}
