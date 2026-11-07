//! Local activity: commands run and signoffs written, kept in
//! `history.json` beside the config, newest first.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};

use fastframe_file::{Access, Writer};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::checks::CheckState;
use crate::config::ConfigError;

/// Events kept on disk.
pub const MAX_EVENTS: usize = 500;

/// Output lines kept per event; earlier ones are counted, not kept.
pub const MAX_OUTPUT_LINES: usize = 400;

/// Output bytes kept per event, so one noisy run cannot bloat the file.
pub const MAX_OUTPUT_BYTES: usize = 32 * 1024;

/// A longer line is cut to this many bytes.
const MAX_LINE_BYTES: usize = 2 * 1024;

/// Only the newest events with output keep it; older ones keep their
/// result. Bounds the file at about this many times [`MAX_OUTPUT_BYTES`].
pub const EVENTS_WITH_OUTPUT: usize = 50;

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
    /// The required signoff checks of a branch were changed.
    Rules,
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
    /// The last lines it printed, oldest first, so the run can be seen
    /// again. Empty for events without output and for older ones.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub output: Vec<String>,
    /// Lines dropped from the front of `output` to keep it small.
    #[serde(skip_serializing_if = "is_zero")]
    pub output_dropped: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// The tail of a run's output, as kept in history: at most
/// [`MAX_OUTPUT_LINES`] lines and about [`MAX_OUTPUT_BYTES`] bytes, the
/// newest ones.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputTail {
    lines: std::collections::VecDeque<String>,
    bytes: usize,
    dropped: usize,
}

impl OutputTail {
    /// Adds a line, dropping the oldest ones past the limits. A very long
    /// line is cut short.
    pub fn push(&mut self, line: &str) {
        let line = if line.len() > MAX_LINE_BYTES {
            let mut end = MAX_LINE_BYTES;
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}…", &line[..end])
        } else {
            line.to_owned()
        };
        self.bytes += line.len();
        self.lines.push_back(line);
        while self.lines.len() > MAX_OUTPUT_LINES || (self.bytes > MAX_OUTPUT_BYTES && self.lines.len() > 1) {
            if let Some(old) = self.lines.pop_front() {
                self.bytes -= old.len();
                self.dropped += 1;
            }
        }
    }

    /// Starts over, as for a new step of a run.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Moves the kept lines into `event`, leaving this empty.
    pub fn take_into(&mut self, event: &mut ActivityEvent) {
        let tail = std::mem::take(self);
        event.output = tail.lines.into();
        event.output_dropped = tail.dropped;
    }
}

/// The history file and its loaded events.
///
/// Appending takes effect at once and hands the file to a background
/// writer, so it never waits for the disk. Call [`HistoryStore::flush`]
/// before the app exits.
#[derive(Debug)]
pub struct HistoryStore {
    path: PathBuf,
    events: RwLock<Vec<ActivityEvent>>,
    writer: Writer,
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
        Ok(Self { path, events: RwLock::new(events), writer: Writer::new() })
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
    /// When the events cannot be serialized; the event is then not kept.
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
        let path = self.path.clone();
        self.writer.write_or(&self.path, bytes, Access::Private, move || {
            log::error!("could not write {}", path.display());
        });
        *guard = next;
        Ok(())
    }

    /// Waits, at most [`fastframe_file::FLUSH_TIMEOUT`], until every event
    /// is on disk. Returns whether it is.
    pub fn flush(&self) -> bool {
        self.writer.flush(fastframe_file::FLUSH_TIMEOUT)
    }
}

fn sort_and_trim(events: &mut Vec<ActivityEvent>) {
    events.sort_by_key(|e| std::cmp::Reverse(e.at));
    events.truncate(MAX_EVENTS);
    for event in events.iter_mut().filter(|e| !e.output.is_empty()).skip(EVENTS_WITH_OUTPUT) {
        event.output = Vec::new();
        event.output_dropped = 0;
    }
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
        assert!(history.flush(), "written");

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

    /// The Go version also logged branch-rule edits; one of them must not
    /// stop the whole file loading.
    #[test]
    fn reads_go_rules_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("history.json");
        std::fs::write(
            &path,
            r#"[{"id":"y","kind":"rules","repoId":"r","repo":"o/r","check":"tests, lint","state":"success",
            "detail":"Updated required signoff checks on main","at":"2026-08-10T08:18:36Z"}]"#,
        )
        .expect("write");
        let events = HistoryStore::open(&path).expect("open").snapshot(0);
        assert_eq!(events[0].kind, ActivityKind::Rules);
    }

    #[test]
    fn output_survives_a_reload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("history.json");
        let history = HistoryStore::open(&path).expect("open");
        let mut tail = OutputTail::default();
        tail.push("$ cargo test");
        tail.push("test result: ok");
        let mut event = ActivityEvent { kind: ActivityKind::Run, ..ActivityEvent::default() };
        tail.take_into(&mut event);
        history.append(event).expect("append");
        assert!(history.flush(), "written");

        let events = HistoryStore::open(&path).expect("reload").snapshot(0);
        assert_eq!(events[0].output, ["$ cargo test", "test result: ok"]);
        assert_eq!(events[0].output_dropped, 0);
    }

    #[test]
    fn events_without_output_leave_it_out_of_the_file() {
        let json = serde_json::to_string(&ActivityEvent::default()).expect("json");
        assert!(!json.contains("output"), "{json}");
    }

    #[test]
    fn the_tail_keeps_the_newest_lines() {
        let mut tail = OutputTail::default();
        for n in 0..MAX_OUTPUT_LINES + 5 {
            tail.push(&n.to_string());
        }
        let mut event = ActivityEvent::default();
        tail.take_into(&mut event);
        assert_eq!(event.output.len(), MAX_OUTPUT_LINES);
        assert_eq!(event.output_dropped, 5);
        assert_eq!(event.output[0], "5");
        assert_eq!(tail, OutputTail::default(), "taken");
    }

    #[test]
    fn the_tail_stays_under_its_byte_limit() {
        let mut tail = OutputTail::default();
        let long = "é".repeat(MAX_LINE_BYTES);
        for _ in 0..40 {
            tail.push(&long);
        }
        let mut event = ActivityEvent::default();
        tail.take_into(&mut event);
        let bytes: usize = event.output.iter().map(String::len).sum();
        assert!(bytes <= MAX_OUTPUT_BYTES, "{bytes}");
        assert!(event.output[0].ends_with('…'), "long lines are cut");
        assert!(event.output_dropped > 0);
    }

    #[test]
    fn only_the_newest_events_keep_their_output() {
        let dir = tempfile::tempdir().expect("tempdir");
        let history = HistoryStore::open(dir.path().join("history.json")).expect("open");
        let now = Timestamp::now();
        for n in 0..=EVENTS_WITH_OUTPUT {
            let at = now - SignedDuration::from_secs(i64::try_from(n).expect("small"));
            let event = ActivityEvent { at, output: vec![n.to_string()], ..ActivityEvent::default() };
            history.append(event).expect("append");
        }
        let events = history.snapshot(0);
        assert_eq!(events.len(), EVENTS_WITH_OUTPUT + 1);
        assert_eq!(events[EVENTS_WITH_OUTPUT - 1].output, [(EVENTS_WITH_OUTPUT - 1).to_string()]);
        assert!(events[EVENTS_WITH_OUTPUT].output.is_empty(), "the oldest lost its output");
    }

    #[test]
    fn history_sits_beside_the_config() {
        assert_eq!(default_history_path(Path::new("/c/Signoff/config.json")), PathBuf::from("/c/Signoff/history.json"));
    }
}
