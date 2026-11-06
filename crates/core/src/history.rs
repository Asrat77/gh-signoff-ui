//! Local activity: commands run and signoffs written, kept in
//! `history.json` beside the config, newest first.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::checks::CheckState;
use crate::config::ConfigError;

/// Events kept on disk.
pub const MAX_EVENTS: usize = 500;

/// What happened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivityKind {
    /// A mapped local command ran.
    Run,
    /// `gh signoff` was written.
    Signoff,
    /// A status read from GitHub.
    #[default]
    Status,
}

/// One entry of activity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ActivityEvent {
    /// Unique per event.
    pub id: String,
    /// What happened.
    pub kind: ActivityKind,
    /// The tracked repository's id.
    pub repo_id: String,
    /// `owner/name`.
    pub repo: String,
    /// The pull request.
    pub pr_number: u64,
    /// Its title.
    pub pr_title: String,
    /// The check's display name.
    pub check: String,
    /// How it ended.
    pub state: CheckState,
    /// Who.
    pub actor: String,
    /// The command run, for [`ActivityKind::Run`].
    pub command: String,
    /// Free text.
    pub detail: String,
    /// The command's exit code.
    pub exit_code: i32,
    /// How long it took.
    pub duration_millis: i64,
    /// When.
    pub at: Timestamp,
}

/// The history file and its loaded events.
///
/// Appending saves at once and waits for the disk, so call it off the UI
/// thread.
#[derive(Debug)]
pub struct HistoryStore {
    path: PathBuf,
    events: RwLock<Vec<ActivityEvent>>,
}

/// `history.json` beside the config file.
pub fn default_history_path(config_path: &Path) -> PathBuf {
    config_path.parent().unwrap_or(Path::new(".")).join("history.json")
}

impl HistoryStore {
    /// Loads `path`, or starts empty if it does not exist.
    ///
    /// # Errors
    ///
    /// When the file exists but cannot be read or parsed.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        let path = path.into();
        let mut events: Vec<ActivityEvent> = match std::fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|source| ConfigError::Parse { path: path.clone(), source })?
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(source) => return Err(ConfigError::Io { action: "reading", path, source }),
        };
        sort_and_trim(&mut events);
        Ok(Self { path, events: RwLock::new(events) })
    }

    /// The newest `limit` events (all of them for 0).
    pub fn snapshot(&self, limit: usize) -> Vec<ActivityEvent> {
        let events = self.events.read().unwrap_or_else(PoisonError::into_inner);
        let limit = if limit == 0 { events.len() } else { limit.min(events.len()) };
        events[..limit].to_vec()
    }

    /// Records `event`, filling its time and id if missing, and saves.
    ///
    /// # Errors
    ///
    /// When the file cannot be written; the event is then not kept.
    pub fn append(&self, mut event: ActivityEvent) -> Result<(), ConfigError> {
        if event.at == Timestamp::UNIX_EPOCH {
            event.at = Timestamp::now();
        }
        if event.id.is_empty() {
            event.id = format!("{}-{}-{}-{}", event.at.as_nanosecond(), event.repo_id, event.pr_number, event.check);
        }
        let mut guard = self.events.write().unwrap_or_else(PoisonError::into_inner);
        let mut next = Vec::with_capacity(guard.len() + 1);
        next.push(event);
        next.extend(guard.iter().cloned());
        sort_and_trim(&mut next);

        let mut bytes = serde_json::to_vec_pretty(&next)
            .map_err(|source| ConfigError::Parse { path: self.path.clone(), source })?;
        bytes.push(b'\n');
        fastframe_file::write_private(&self.path, bytes).map_err(|source| ConfigError::Io {
            action: "writing",
            path: self.path.clone(),
            source,
        })?;
        *guard = next;
        Ok(())
    }
}

fn sort_and_trim(events: &mut Vec<ActivityEvent>) {
    events.sort_by_key(|e| std::cmp::Reverse(e.at));
    events.truncate(MAX_EVENTS);
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::SignedDuration;

    #[test]
    fn persists_newest_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("history.json");
        let history = HistoryStore::open(&path).expect("open");
        let now = Timestamp::now();
        history
            .append(ActivityEvent {
                id: "old".into(),
                at: now - SignedDuration::from_hours(1),
                ..ActivityEvent::default()
            })
            .expect("append");
        history.append(ActivityEvent { id: "new".into(), at: now, ..ActivityEvent::default() }).expect("append");

        let ids: Vec<String> =
            HistoryStore::open(&path).expect("reload").snapshot(10).into_iter().map(|e| e.id).collect();
        assert_eq!(ids, ["new", "old"]);
    }

    #[test]
    fn fills_time_and_id_and_trims() {
        let dir = tempfile::tempdir().expect("tempdir");
        let history = HistoryStore::open(dir.path().join("history.json")).expect("open");
        history
            .append(ActivityEvent {
                repo_id: "r".into(),
                pr_number: 7,
                check: "tests".into(),
                ..ActivityEvent::default()
            })
            .expect("append");
        let event = &history.snapshot(1)[0];
        assert!(event.at > Timestamp::UNIX_EPOCH);
        assert!(event.id.ends_with("-r-7-tests"));
        assert_eq!(history.snapshot(0).len(), 1);
    }

    /// The Go version's file must keep loading.
    #[test]
    fn reads_the_go_history_format() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("history.json");
        std::fs::write(
            &path,
            r#"[{"id":"x","kind":"run","repoId":"r","repo":"o/r","prNumber":3,"prTitle":"t","check":"tests",
            "state":"failure","actor":"you","command":"bin/test","detail":"","exitCode":1,"durationMillis":1200,
            "at":"2026-08-10T08:18:36.123456789+02:00"}]"#,
        )
        .expect("write");
        let events = HistoryStore::open(&path).expect("open").snapshot(0);
        assert_eq!(events[0].kind, ActivityKind::Run);
        assert_eq!(events[0].state, CheckState::Failure);
        assert_eq!(events[0].exit_code, 1);
    }

    #[test]
    fn history_sits_beside_the_config() {
        assert_eq!(default_history_path(Path::new("/c/Signoff/config.json")), PathBuf::from("/c/Signoff/history.json"));
    }
}
