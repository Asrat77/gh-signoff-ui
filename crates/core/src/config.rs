//! The tracked repositories and their settings, kept in `config.json`.
//!
//! The file format is the one the Go version wrote, so an existing config
//! keeps working.

use std::collections::BTreeMap;
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
    /// Seconds between background refreshes.
    pub refresh_seconds: u32,
    /// Tell the person with a desktop notification when one of their pull
    /// requests starts failing or becomes ready, or a local run finishes.
    pub notifications: bool,
    /// Tracked checkouts, sorted by GitHub name.
    pub repositories: Vec<TrackedRepository>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self { version: CURRENT_VERSION, refresh_seconds: 30, notifications: true, repositories: Vec::new() }
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
        for repo in &mut self.repositories {
            repo.normalize();
        }
        self.repositories.sort_by_key(|r| r.name_with_owner.to_lowercase());
    }
}

impl TrackedRepository {
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
            commands: BTreeMap::new(),
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
            repo.commands = update.commands;
            repo.normalize();
            Ok(repo.clone())
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
