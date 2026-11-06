//! Keeps Signoff's worktrees to what the config wants.
//!
//! A cleanup runs once at startup, which removes worktrees whose checkouts
//! are gone, and again after every config change, which covers turning the
//! setting off and untracking a repository. Each removal spawns `git`, so
//! it runs on its own thread; changes that arrive while one runs are
//! folded into the next.

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use signoff_core::{ConfigStore, Runner, Worktrees};

/// Starts cleaning `worktrees` now and after each change to `config`.
pub(crate) fn keep_tidy(worktrees: Worktrees, runner: Arc<dyn Runner>, config: &Arc<ConfigStore>) {
    let (tx, rx) = mpsc::channel::<()>();
    let store = config.clone();
    let spawned = thread::Builder::new().name("signoff-worktrees".into()).spawn(move || {
        while rx.recv().is_ok() {
            while rx.try_recv().is_ok() {}
            for path in worktrees.retain(runner.as_ref(), &store.snapshot()) {
                log::info!("removed the worktree {}", path.display());
            }
        }
    });
    if let Err(err) = spawned {
        log::error!("could not start the worktree cleanup thread: {err}");
        return;
    }
    let _ = tx.send(());
    config.subscribe(move || {
        let _ = tx.send(());
    });
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use signoff_core::{AppConfig, SystemRunner, TrackedRepository};

    use super::*;

    fn wait_for(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn folder(path: &Path) {
        std::fs::create_dir_all(path).expect("mkdir");
        std::fs::write(path.join("file"), "x").expect("write");
    }

    #[test]
    fn cleans_up_at_startup_and_when_the_setting_is_turned_off() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("worktrees");
        let checkout = dir.path().join("checkout");
        std::fs::create_dir_all(&checkout).expect("mkdir");
        let repo = TrackedRepository { id: "kept".into(), path: checkout, ..TrackedRepository::default() };
        // A worktree of a repository no longer tracked, one still wanted,
        // and a file that is not a worktree at all.
        folder(&root.join("gone"));
        folder(&root.join("kept"));
        std::fs::write(root.join("notes.txt"), "mine").expect("write");
        let config = Arc::new(ConfigStore::with_config(
            dir.path().join("config.json"),
            AppConfig { isolated_worktrees: true, repositories: vec![repo], ..AppConfig::default() },
        ));

        keep_tidy(Worktrees::new(&root), Arc::new(SystemRunner::default()), &config);
        wait_for("the startup cleanup", || !root.join("gone").exists());
        assert!(root.join("kept").is_dir());

        config.set_isolated_worktrees(false).expect("save");
        wait_for("the cleanup after turning it off", || !root.join("kept").exists());
        assert!(root.join("notes.txt").is_file(), "only worktree folders are removed");
    }
}
