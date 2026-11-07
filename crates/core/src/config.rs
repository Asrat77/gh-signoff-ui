//! The tracked repositories and their settings, kept in `config.json`.
//!
//! The file format is the one the Go version wrote, so an existing config
//! keeps working.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use fastframe_file::{Access, Writer};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::gh::{Gh, GhError};

/// The config format version this build writes.
pub const CURRENT_VERSION: u32 = 1;
/// The longest a mapped command may run.
pub const MAX_TIMEOUT_SECONDS: u32 = 7200;
/// The timeout of a mapped command that names none.
pub const DEFAULT_TIMEOUT_SECONDS: u32 = 1800;

/// The local work that must succeed before a signoff is written. The command
/// is the user's, runs in the tracked checkout, and is never taken from
/// anywhere but the config.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CommandMapping {
    /// The shell command.
    pub command: String,
    /// How long it may run.
    pub timeout_seconds: u32,
}

/// A command Signoff set up for a check on its own, from the repository's
/// CI config, and where it found it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DetectedCommand {
    /// The command line it set up.
    pub command: String,
    /// The file it came from, such as `.github/workflows/ci.yml`.
    pub source: String,
}

/// One local checkout Signoff watches.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TrackedRepository {
    /// Derived from the canonical path, so it is stable across launches.
    pub id: String,
    /// The checkout.
    pub path: PathBuf,
    /// `owner/name`.
    pub name_with_owner: String,
    /// Its default branch.
    pub default_branch: String,
    /// Whether it is refreshed.
    pub enabled: bool,
    /// Local commands by check name (`tests`, `lint`…).
    pub commands: BTreeMap<String, CommandMapping>,
    /// The commands Signoff set up from the CI config, by check name. A
    /// command is still Signoff's while it equals the one in `commands`;
    /// once the person edits it, it is theirs and detection leaves it.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub detected: BTreeMap<String, DetectedCommand>,
    /// Checks whose detected command the person removed, so it is not set
    /// up again.
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub dismissed: BTreeSet<String>,
}

/// The editable part of a [`TrackedRepository`]. Its identity (id, path,
/// GitHub name) cannot be changed after it was added.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepositoryUpdate {
    /// Whether it is refreshed.
    pub enabled: bool,
    /// Local commands by check name.
    pub commands: BTreeMap<String, CommandMapping>,
}

/// The whole config file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppConfig {
    /// The format version.
    pub version: u32,
    /// Seconds between background refreshes, from before Signoff picked
    /// its own cadence. No longer read; kept so the setting survives a
    /// save for an older build reading the same file.
    pub refresh_seconds: u32,
    /// Tell the person with a desktop notification when one of their pull
    /// requests starts failing or becomes ready, or a local run finishes.
    pub notifications: bool,
    /// Whether the global shortcut that opens the popover is on.
    pub shortcut_enabled: bool,
    /// The global shortcut, such as `Alt+Shift+KeyS`; empty means the
    /// app's default. An invalid string also falls back to the default.
    pub shortcut: String,
    /// Run a pull request's checks in a separate worktree that Signoff
    /// manages, at the pull request's head, instead of requiring the
    /// tracked checkout to be there. Off unless the person turns it on.
    pub isolated_worktrees: bool,
    /// Tracked checkouts, sorted by GitHub name.
    pub repositories: Vec<TrackedRepository>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            refresh_seconds: 30,
            notifications: true,
            shortcut_enabled: true,
            shortcut: String::new(),
            isolated_worktrees: false,
            repositories: Vec::new(),
        }
    }
}

impl AppConfig {
    /// Fills defaults, bounds values and sorts, as every load and save does.
    pub fn normalize(&mut self) {
        if self.version == 0 {
            self.version = CURRENT_VERSION;
        }
        if self.refresh_seconds < 10 {
            self.refresh_seconds = 30;
        }
        self.shortcut = self.shortcut.trim().to_owned();
        for repo in &mut self.repositories {
            repo.normalize();
        }
        self.repositories.sort_by_key(|r| r.name_with_owner.to_lowercase());
    }
}

impl TrackedRepository {
    /// Where the command for `check` was detected, while it is still the
    /// one Signoff set up.
    pub fn detected_source(&self, check: &str) -> Option<&str> {
        let mapping = self.commands.get(check)?;
        self.detected.get(check).filter(|d| d.command == mapping.command).map(|d| d.source.as_str())
    }

    /// Derives a missing id and bounds every command's timeout.
    pub fn normalize(&mut self) {
        if self.id.is_empty() && !self.path.as_os_str().is_empty() {
            self.id = repository_id(&self.path);
        }
        for mapping in self.commands.values_mut() {
            mapping.command = mapping.command.trim().to_owned();
            if mapping.timeout_seconds == 0 {
                mapping.timeout_seconds = DEFAULT_TIMEOUT_SECONDS;
            }
            mapping.timeout_seconds = mapping.timeout_seconds.min(MAX_TIMEOUT_SECONDS);
        }
    }
}

/// A stable id for a checkout: the first eight bytes of the SHA-256 of its
/// cleaned path, in hex.
pub fn repository_id(path: &Path) -> String {
    let cleaned = clean(path);
    let digest = Sha256::digest(cleaned.to_string_lossy().as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Lexically removes `.`, `..` and repeated or trailing separators.
fn clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// Why a config operation failed.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Reading or writing the file.
    #[error("{action} {path}: {source}")]
    Io {
        /// What was being done.
        action: &'static str,
        /// The file.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// The file is not a config.
    #[error("parsing {path}: {source}")]
    Parse {
        /// The file.
        path: PathBuf,
        /// Why.
        source: serde_json::Error,
    },
    /// The path given to add is unusable.
    #[error("{0}")]
    InvalidPath(String),
    /// The checkout is not a GitHub repository `gh` can read.
    #[error("reading GitHub repository: {0}")]
    Gh(#[from] GhError),
    /// Already tracked.
    #[error("repository is already tracked")]
    AlreadyTracked,
    /// No repository has that id.
    #[error("no tracked repository has that id")]
    NotFound,
    /// The refresh interval is out of range.
    #[error("refresh interval must be between 10 and 3600 seconds")]
    InvalidRefresh,
}

/// The config file and its loaded contents.
///
/// Every change takes effect at once and is handed to a background writer,
/// which replaces the file atomically, readable only by the user. Changing
/// methods never wait for the disk, so the UI thread may call them; call
/// [`ConfigStore::flush`] before the app exits.
///
/// Whoever shows state derived from the config can
/// [`subscribe`](ConfigStore::subscribe) to hear about each change at once,
/// rather than waiting for the next refresh to notice it.
#[derive(Debug)]
pub struct ConfigStore {
    path: PathBuf,
    config: RwLock<AppConfig>,
    writer: Writer,
    listeners: Listeners,
}

/// Callbacks run after each change.
#[derive(Default)]
struct Listeners(Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>);

impl std::fmt::Debug for Listeners {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.0.lock().map(|l| l.len()).unwrap_or_default();
        f.debug_tuple("Listeners").field(&count).finish()
    }
}

impl ConfigStore {
    /// Loads `path`, or starts from defaults if it does not exist.
    ///
    /// # Errors
    ///
    /// When the file exists but cannot be read or parsed.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        let path = path.into();
        let config = match std::fs::read(&path) {
            Ok(bytes) => {
                let mut config: AppConfig = serde_json::from_slice(&bytes)
                    .map_err(|source| ConfigError::Parse { path: path.clone(), source })?;
                config.normalize();
                config
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => AppConfig::default(),
            Err(source) => return Err(ConfigError::Io { action: "reading", path, source }),
        };
        Ok(Self { path, config: RwLock::new(config), writer: Writer::new(), listeners: Listeners::default() })
    }

    /// A store that holds `config` and saves to `path`.
    pub fn with_config(path: impl Into<PathBuf>, mut config: AppConfig) -> Self {
        config.normalize();
        Self { path: path.into(), config: RwLock::new(config), writer: Writer::new(), listeners: Listeners::default() }
    }

    /// Waits, at most [`fastframe_file::FLUSH_TIMEOUT`], until every change
    /// is on disk. Returns whether it is.
    pub fn flush(&self) -> bool {
        self.writer.flush(fastframe_file::FLUSH_TIMEOUT)
    }

    /// Calls `listener` after every change from now on, on the thread that
    /// made it and after the new config is readable. Keep it quick: the UI
    /// thread makes most changes.
    pub fn subscribe(&self, listener: impl Fn() + Send + Sync + 'static) {
        self.listeners.0.lock().unwrap_or_else(PoisonError::into_inner).push(Arc::new(listener));
    }

    /// The config file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A copy of the config.
    pub fn snapshot(&self) -> AppConfig {
        self.config.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The tracked repository with `id`.
    pub fn find_repository(&self, id: &str) -> Option<TrackedRepository> {
        self.config.read().unwrap_or_else(PoisonError::into_inner).repositories.iter().find(|r| r.id == id).cloned()
    }

    /// Starts tracking the checkout at `path`, after checking it is a git
    /// repository on GitHub.
    ///
    /// # Errors
    ///
    /// When it is not, is already tracked, or the config cannot be saved.
    pub fn add_repository(&self, gh: &Gh, path: &Path) -> Result<TrackedRepository, ConfigError> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|err| ConfigError::InvalidPath(format!("opening repository path: {err}")))?;
        // `canonicalize` gives `\\?\C:\…` on Windows; keep the familiar form.
        let canonical = strip_verbatim(canonical);
        if !canonical.is_dir() {
            return Err(ConfigError::InvalidPath("repository path is not a directory".into()));
        }
        let info = gh.repo(&canonical)?;
        let mut repo = TrackedRepository {
            id: repository_id(&canonical),
            path: canonical,
            name_with_owner: info.name_with_owner,
            default_branch: info.default_branch,
            enabled: true,
            ..TrackedRepository::default()
        };
        repo.normalize();
        self.change(|config| {
            if config.repositories.iter().any(|r| r.id == repo.id) {
                return Err(ConfigError::AlreadyTracked);
            }
            config.repositories.push(repo.clone());
            Ok(repo)
        })
    }

    /// Changes the editable settings of the repository with `id`.
    ///
    /// # Errors
    ///
    /// When there is none, or the config cannot be saved.
    pub fn update_repository(&self, id: &str, update: RepositoryUpdate) -> Result<TrackedRepository, ConfigError> {
        self.change(|config| {
            let repo = config.repositories.iter_mut().find(|r| r.id == id).ok_or(ConfigError::NotFound)?;
            repo.enabled = update.enabled;
            // Removing a command Signoff set up says not to set it up again;
            // giving the check a command of one's own lifts that.
            for (check, detected) in &repo.detected {
                if repo.commands.get(check).is_some_and(|m| m.command == detected.command)
                    && !update.commands.contains_key(check)
                {
                    repo.dismissed.insert(check.clone());
                }
            }
            repo.dismissed.retain(|check| !update.commands.contains_key(check));
            repo.commands = update.commands;
            repo.normalize();
            Ok(repo.clone())
        })
    }

    /// Sets up the commands in `found`, detected from the CI config of the
    /// repository with `id`, for checks that have no command, or whose
    /// command Signoff set up before and the CI config has since changed.
    /// Leaves the person's own commands and dismissed checks alone. Saves
    /// only when something changes; returns whether it did.
    ///
    /// # Errors
    ///
    /// When the config cannot be saved.
    pub fn adopt_detected(&self, id: &str, found: &BTreeMap<String, DetectedCommand>) -> Result<bool, ConfigError> {
        let Some(repo) = self.find_repository(id) else { return Ok(false) };
        if adopt(&repo, found).is_none() {
            return Ok(false);
        }
        self.change(|config| {
            let Some(repo) = config.repositories.iter_mut().find(|r| r.id == id) else { return Ok(false) };
            match adopt(repo, found) {
                Some(next) => {
                    *repo = next;
                    Ok(true)
                }
                None => Ok(false),
            }
        })
    }

    /// Stops tracking the repository with `id`.
    ///
    /// # Errors
    ///
    /// When there is none, or the config cannot be saved.
    pub fn remove_repository(&self, id: &str) -> Result<(), ConfigError> {
        self.change(|config| {
            let index = config.repositories.iter().position(|r| r.id == id).ok_or(ConfigError::NotFound)?;
            config.repositories.remove(index);
            Ok(())
        })
    }

    /// Sets the background refresh interval.
    ///
    /// # Errors
    ///
    /// When it is outside 10–3600 seconds, or the config cannot be saved.
    pub fn set_refresh_seconds(&self, seconds: u32) -> Result<(), ConfigError> {
        if !(10..=3600).contains(&seconds) {
            return Err(ConfigError::InvalidRefresh);
        }
        self.change(|config| {
            config.refresh_seconds = seconds;
            Ok(())
        })
    }

    /// Turns desktop notifications on or off.
    ///
    /// # Errors
    ///
    /// When the config cannot be saved.
    pub fn set_notifications(&self, on: bool) -> Result<(), ConfigError> {
        self.change(|config| {
            config.notifications = on;
            Ok(())
        })
    }

    /// Turns the global shortcut on or off.
    ///
    /// # Errors
    ///
    /// When the config cannot be saved.
    pub fn set_shortcut_enabled(&self, on: bool) -> Result<(), ConfigError> {
        self.change(|config| {
            config.shortcut_enabled = on;
            Ok(())
        })
    }

    /// Turns verifying in a separate, Signoff-managed worktree on or off.
    ///
    /// # Errors
    ///
    /// When the config cannot be saved.
    pub fn set_isolated_worktrees(&self, on: bool) -> Result<(), ConfigError> {
        self.change(|config| {
            config.isolated_worktrees = on;
            Ok(())
        })
    }

    /// Sets the global shortcut; empty means the app's default. The app
    /// checks that it parses: the config only keeps the text.
    ///
    /// # Errors
    ///
    /// When the config cannot be saved.
    pub fn set_shortcut(&self, shortcut: &str) -> Result<(), ConfigError> {
        self.change(|config| {
            shortcut.clone_into(&mut config.shortcut);
            Ok(())
        })
    }

    /// Applies `edit` and saves. The change is kept only if both succeed.
    fn change<T>(&self, edit: impl FnOnce(&mut AppConfig) -> Result<T, ConfigError>) -> Result<T, ConfigError> {
        let mut guard = self.config.write().unwrap_or_else(PoisonError::into_inner);
        let mut next = guard.clone();
        let value = edit(&mut next)?;
        next.normalize();
        self.save(&next)?;
        *guard = next;
        drop(guard);
        // Copied out so a listener may subscribe or read the config.
        let listeners = self.listeners.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
        for listener in listeners {
            listener();
        }
        Ok(value)
    }

    /// Hands `config` to the writer. Only serializing can fail here; a
    /// failed write is logged from the writer's thread.
    fn save(&self, config: &AppConfig) -> Result<(), ConfigError> {
        let mut bytes = serde_json::to_vec_pretty(config)
            .map_err(|source| ConfigError::Parse { path: self.path.clone(), source })?;
        bytes.push(b'\n');
        let path = self.path.clone();
        self.writer.write_or(&self.path, bytes, Access::Private, move || {
            log::error!("could not write {}", path.display());
        });
        Ok(())
    }
}

/// `repo` with the commands in `found` set up, or `None` when nothing
/// changes.
fn adopt(repo: &TrackedRepository, found: &BTreeMap<String, DetectedCommand>) -> Option<TrackedRepository> {
    let mut next = repo.clone();
    for (check, detected) in found {
        if detected.command.trim().is_empty() || next.dismissed.contains(check) {
            continue;
        }
        let ours = next.detected_source(check).is_some();
        match next.commands.get_mut(check) {
            None => {
                let mapping = CommandMapping { command: detected.command.clone(), timeout_seconds: 0 };
                next.commands.insert(check.clone(), mapping);
            }
            Some(mapping) if mapping.command.trim().is_empty() || ours => mapping.command.clone_from(&detected.command),
            // The person's own command, or the same one typed by hand.
            Some(mapping) if mapping.command != detected.command => continue,
            Some(_) => {}
        }
        next.detected.insert(check.clone(), detected.clone());
    }
    next.normalize();
    (next != *repo).then_some(next)
}

fn strip_verbatim(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\")
            && !rest.starts_with("UNC\\")
        {
            return PathBuf::from(rest);
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked() -> TrackedRepository {
        TrackedRepository {
            id: "repo-1".into(),
            path: "/tmp/example".into(),
            name_with_owner: "acme/example".into(),
            default_branch: "main".into(),
            enabled: true,
            commands: BTreeMap::from([(
                "tests".into(),
                CommandMapping { command: "bin/test".into(), timeout_seconds: 90 },
            )]),
            ..TrackedRepository::default()
        }
    }

    #[test]
    fn saves_from_the_ui_thread_without_waiting_for_the_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.json");
        let store = ConfigStore::with_config(&path, AppConfig::default());
        // A durable write on this thread would now panic in a debug build.
        fastframe_file::mark_ui_thread();
        store.set_refresh_seconds(120).expect("save");
        assert!(store.flush(), "written");
        assert_eq!(ConfigStore::open(&path).expect("reload").snapshot().refresh_seconds, 120);
    }

    #[test]
    fn tells_subscribers_about_each_change_after_it_applies() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(ConfigStore::with_config(dir.path().join("config.json"), AppConfig::default()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (weak, log) = (Arc::downgrade(&store), seen.clone());
        store.subscribe(move || {
            let seconds = weak.upgrade().map(|s| s.snapshot().refresh_seconds);
            log.lock().expect("seen").push(seconds);
        });
        store.set_refresh_seconds(45).expect("save");
        assert!(store.set_refresh_seconds(1).is_err());
        assert!(store.remove_repository("missing").is_err());
        assert_eq!(*seen.lock().expect("seen"), [Some(45)], "only changes that applied, and readable");
    }

    #[test]
    fn persists_repository_settings_privately() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.json");
        let store =
            ConfigStore::with_config(&path, AppConfig { repositories: vec![tracked()], ..AppConfig::default() });
        store.set_refresh_seconds(45).expect("save");
        assert!(store.flush(), "written");

        let reloaded = ConfigStore::open(&path).expect("reload").snapshot();
        assert_eq!(reloaded.refresh_seconds, 45);
        assert_eq!(reloaded.repositories, vec![tracked()]);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
            assert_eq!(mode & 0o077, 0, "config permissions {mode:o} should be private");
        }
    }

    /// Notifications are on unless a config says otherwise, and the switch
    /// is kept.
    #[test]
    fn notifications_default_on_for_old_configs_and_the_switch_is_saved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"version":1,"refreshSeconds":30,"repositories":[]}"#).expect("write");
        let store = ConfigStore::open(&path).expect("open");
        assert!(store.snapshot().notifications, "an old file has no setting: on");
        store.set_notifications(false).expect("save");
        assert!(store.flush());
        assert!(!ConfigStore::open(&path).expect("reload").snapshot().notifications);
    }

    /// The shortcut is on, with the app's default, unless a config says
    /// otherwise, and its edits are kept.
    #[test]
    fn shortcut_defaults_for_old_configs_and_edits_are_saved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"version":1,"refreshSeconds":30,"repositories":[]}"#).expect("write");
        let store = ConfigStore::open(&path).expect("open");
        assert!(store.snapshot().shortcut_enabled);
        assert_eq!(store.snapshot().shortcut, "");
        store.set_shortcut_enabled(false).expect("save");
        store.set_shortcut(" Alt+Shift+KeyS ").expect("save");
        assert!(store.flush());
        let loaded = ConfigStore::open(&path).expect("reload").snapshot();
        assert!(!loaded.shortcut_enabled);
        assert_eq!(loaded.shortcut, "Alt+Shift+KeyS");
    }

    /// Verifying in a separate worktree is off unless a config says
    /// otherwise, and the switch is kept.
    #[test]
    fn isolated_worktrees_default_off_for_old_configs_and_the_switch_is_saved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"version":1,"refreshSeconds":30,"repositories":[]}"#).expect("write");
        let store = ConfigStore::open(&path).expect("open");
        assert!(!store.snapshot().isolated_worktrees, "an old file has no setting: off");
        store.set_isolated_worktrees(true).expect("save");
        assert!(store.flush());
        assert!(std::fs::read_to_string(&path).expect("read").contains(r#""isolatedWorktrees": true"#));
        assert!(ConfigStore::open(&path).expect("reload").snapshot().isolated_worktrees);
    }

    /// The Go version's file must keep loading.
    #[test]
    fn reads_the_go_config_format() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"version":1,"refreshSeconds":5,"repositories":[{"id":"abc","path":"/w/r","nameWithOwner":"o/r",
            "defaultBranch":"main","enabled":true,"pullRequestScope":"weird","commands":{"tests":{"command":" bin/test ","timeoutSeconds":0}}}]}"#,
        )
        .expect("write");
        let config = ConfigStore::open(&path).expect("open").snapshot();
        assert_eq!(config.refresh_seconds, 30, "below 10 falls back to the default");
        let repo = &config.repositories[0];
        assert_eq!(repo.name_with_owner, "o/r", "the retired scope field is ignored");
        assert_eq!(
            repo.commands["tests"],
            CommandMapping { command: "bin/test".into(), timeout_seconds: DEFAULT_TIMEOUT_SECONDS }
        );
    }

    /// A file written before the scope was dropped keeps loading, and saving
    /// it does not bring the field back.
    #[test]
    fn an_old_scope_field_is_ignored_and_not_written_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"version":1,"refreshSeconds":30,"repositories":[{"id":"a","path":"/w/a","nameWithOwner":"o/a",
            "defaultBranch":"main","enabled":true,"pullRequestScope":"all"},
            {"id":"b","path":"/w/b","nameWithOwner":"o/b","defaultBranch":"main","enabled":false,"pullRequestScope":"mine"}]}"#,
        )
        .expect("write");
        let store = ConfigStore::open(&path).expect("open");
        let repos = store.snapshot().repositories;
        assert_eq!(repos.len(), 2);
        assert!(repos[0].enabled && !repos[1].enabled);
        store.set_refresh_seconds(60).expect("save");
        assert!(store.flush(), "written");
        assert!(!std::fs::read_to_string(&path).expect("read").contains("pullRequestScope"));
        assert_eq!(ConfigStore::open(&path).expect("reload").snapshot().repositories, repos);
    }

    #[test]
    fn a_missing_file_is_the_default_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(ConfigStore::open(dir.path().join("none.json")).expect("open").snapshot(), AppConfig::default());
    }

    #[test]
    fn command_timeout_is_bounded() {
        let mut repo = TrackedRepository {
            commands: BTreeMap::from([(
                "tests".into(),
                CommandMapping { command: " bin/test ".into(), timeout_seconds: 100_000 },
            )]),
            ..TrackedRepository::default()
        };
        repo.normalize();
        assert_eq!(
            repo.commands["tests"],
            CommandMapping { command: "bin/test".into(), timeout_seconds: MAX_TIMEOUT_SECONDS }
        );
    }

    /// An update carries no path or name, so a repository's identity cannot
    /// be swapped for another checkout.
    #[test]
    fn update_keeps_repository_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::with_config(
            dir.path().join("config.json"),
            AppConfig { repositories: vec![tracked()], ..AppConfig::default() },
        );
        let updated = store
            .update_repository("repo-1", RepositoryUpdate { enabled: false, ..RepositoryUpdate::default() })
            .expect("update");
        assert_eq!(updated.path, PathBuf::from("/tmp/example"));
        assert_eq!(updated.name_with_owner, "acme/example");
        assert!(!updated.enabled);
        assert!(matches!(store.update_repository("nope", RepositoryUpdate::default()), Err(ConfigError::NotFound)));
    }

    fn detected(command: &str) -> DetectedCommand {
        DetectedCommand { command: command.into(), source: ".github/workflows/ci.yml".into() }
    }

    fn command(store: &ConfigStore, check: &str) -> Option<String> {
        store.find_repository("repo-1").and_then(|repo| repo.commands.get(check).map(|m| m.command.clone()))
    }

    #[test]
    fn detected_commands_fill_gaps_and_follow_ci_but_never_replace_the_persons() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::with_config(
            dir.path().join("config.json"),
            AppConfig { repositories: vec![tracked()], ..AppConfig::default() },
        );
        let found =
            BTreeMap::from([("tests".to_owned(), detected("cargo test")), ("lint".into(), detected("cargo clippy"))]);
        assert!(store.adopt_detected("repo-1", &found).expect("adopt"));
        assert_eq!(command(&store, "tests").as_deref(), Some("bin/test"), "the person's own command stays");
        assert_eq!(command(&store, "lint").as_deref(), Some("cargo clippy"));
        let repo = store.find_repository("repo-1").expect("repo");
        assert_eq!(repo.detected_source("lint"), Some(".github/workflows/ci.yml"));
        assert_eq!(repo.detected_source("tests"), None);
        assert_eq!(repo.commands["lint"].timeout_seconds, DEFAULT_TIMEOUT_SECONDS);
        assert!(!store.adopt_detected("repo-1", &found).expect("adopt"), "nothing new: nothing saved");

        let changed = BTreeMap::from([("lint".to_owned(), detected("cargo clippy -- -D warnings"))]);
        assert!(store.adopt_detected("repo-1", &changed).expect("adopt"));
        assert_eq!(command(&store, "lint").as_deref(), Some("cargo clippy -- -D warnings"), "CI changed");

        // Edited by hand: the person's from now on.
        let mut commands = store.find_repository("repo-1").expect("repo").commands;
        commands.get_mut("lint").expect("lint").command = "make lint".into();
        store.update_repository("repo-1", RepositoryUpdate { enabled: true, commands }).expect("update");
        assert!(store.find_repository("repo-1").expect("repo").detected_source("lint").is_none());
        assert!(!store.adopt_detected("repo-1", &found).expect("adopt"));
        assert_eq!(command(&store, "lint").as_deref(), Some("make lint"));
        assert!(!store.adopt_detected("nope", &found).expect("unknown repositories are skipped"));
    }

    #[test]
    fn a_removed_detected_command_stays_removed_until_the_person_adds_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::with_config(
            dir.path().join("config.json"),
            AppConfig { repositories: vec![tracked()], ..AppConfig::default() },
        );
        let found = BTreeMap::from([("lint".to_owned(), detected("cargo clippy"))]);
        store.adopt_detected("repo-1", &found).expect("adopt");

        let mut commands = store.find_repository("repo-1").expect("repo").commands;
        commands.remove("lint");
        store
            .update_repository("repo-1", RepositoryUpdate { enabled: true, commands: commands.clone() })
            .expect("update");
        assert!(store.find_repository("repo-1").expect("repo").dismissed.contains("lint"));
        assert!(!store.adopt_detected("repo-1", &found).expect("adopt"));
        assert_eq!(command(&store, "lint"), None);

        commands.insert("lint".into(), CommandMapping { command: "make lint".into(), timeout_seconds: 60 });
        store.update_repository("repo-1", RepositoryUpdate { enabled: true, commands }).expect("update");
        assert!(store.find_repository("repo-1").expect("repo").dismissed.is_empty());

        // Both survive a reload, and an empty set is not written at all.
        store.flush();
        let saved = std::fs::read_to_string(dir.path().join("config.json")).expect("saved");
        assert!(saved.contains("\"detected\"") && !saved.contains("\"dismissed\""), "{saved}");
    }

    #[test]
    fn remove_and_refresh_bounds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::with_config(
            dir.path().join("config.json"),
            AppConfig { repositories: vec![tracked()], ..AppConfig::default() },
        );
        assert!(matches!(store.set_refresh_seconds(5), Err(ConfigError::InvalidRefresh)));
        store.remove_repository("repo-1").expect("remove");
        assert!(store.snapshot().repositories.is_empty());
        assert!(matches!(store.remove_repository("repo-1"), Err(ConfigError::NotFound)));
    }

    #[test]
    fn repository_ids_are_stable_across_spellings() {
        assert_eq!(repository_id(Path::new("/work/repo")), repository_id(Path::new("/work/./x/../repo/")));
        assert_eq!(repository_id(Path::new("/work/repo")).len(), 16);
    }
}
