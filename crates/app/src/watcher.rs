//! Watches what each tracked checkout has checked out.
//!
//! Checking out a pull request's branch in a terminal should flip its page
//! to "Run & sign off" at once, not on the next timed read. For every
//! watched repository this watches its git directory (for `HEAD`), the
//! shared one (for `packed-refs`) and its branches, with the recommended
//! watcher for the OS, never the working tree. A change is reported as the
//! repositories it can affect; the worker debounces and reads them.
//!
//! A watcher that cannot start or a folder that cannot be watched is
//! logged and skipped: timed reads still notice the change, later.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use notify::event::{EventKind, ModifyKind};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher as _};
use signoff_core::{WatchPaths, watch_paths};

/// Which repository each set of paths belongs to.
type Routes = Arc<Mutex<Vec<(String, WatchPaths)>>>;

/// The watches on every tracked checkout.
pub(crate) struct GitWatcher {
    watcher: Option<RecommendedWatcher>,
    routes: Routes,
    /// What is watched now, each with whether its subfolders are.
    watched: BTreeSet<(PathBuf, bool)>,
}

impl GitWatcher {
    /// Starts a watcher that calls `changed` with the repositories a change
    /// may affect, on the watcher's own thread. Nothing is watched until
    /// [`GitWatcher::watch`].
    pub(crate) fn new(changed: impl Fn(Vec<String>) + Send + 'static) -> Self {
        let routes = Routes::default();
        let handler = {
            let routes = routes.clone();
            move |event: notify::Result<Event>| {
                let event = match event {
                    Ok(event) => event,
                    Err(err) => {
                        log::debug!("git watcher: {err}");
                        return;
                    }
                };
                if !moves_files(&event.kind) {
                    return;
                }
                let routes = routes.lock().unwrap_or_else(PoisonError::into_inner);
                let ids = affected(&routes, &event.paths);
                drop(routes);
                if !ids.is_empty() {
                    changed(ids);
                }
            }
        };
        let watcher = match notify::recommended_watcher(handler) {
            Ok(watcher) => Some(watcher),
            Err(err) => {
                log::warn!("not watching checkouts for changes: {err}");
                None
            }
        };
        Self { watcher, routes, watched: BTreeSet::new() }
    }

    /// Watches exactly `repositories` (id and checkout), adding and
    /// dropping watches to match.
    pub(crate) fn watch(&mut self, repositories: impl IntoIterator<Item = (String, PathBuf)>) {
        let routes: Vec<(String, WatchPaths)> = repositories
            .into_iter()
            .filter_map(|(id, path)| {
                let paths = watch_paths(&path);
                if paths.is_none() {
                    log::debug!("not watching {}: no git directory", path.display());
                }
                Some((id, paths?))
            })
            .collect();
        let wanted: BTreeSet<(PathBuf, bool)> = routes.iter().flat_map(|(_, paths)| paths.targets()).collect();
        *self.routes.lock().unwrap_or_else(PoisonError::into_inner) = routes;
        let Some(watcher) = &mut self.watcher else { return };
        for (path, _) in self.watched.difference(&wanted) {
            if let Err(err) = watcher.unwatch(path) {
                log::debug!("unwatching {}: {err}", path.display());
            }
        }
        let mut watched = BTreeSet::new();
        for target in &wanted {
            let (path, recursive) = target;
            if self.watched.contains(target) {
                watched.insert(target.clone());
                continue;
            }
            let mode = if *recursive { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive };
            match watcher.watch(path, mode) {
                Ok(()) => {
                    watched.insert(target.clone());
                }
                Err(err) => log::warn!("not watching {}: {err}", path.display()),
            }
        }
        self.watched = watched;
    }

    /// What is watched now, for tests.
    #[cfg(test)]
    fn watched(&self) -> Vec<(PathBuf, bool)> {
        self.watched.iter().cloned().collect()
    }
}

/// Whether an event can change a file's contents or name. Opening and
/// reading are not, which matters: every read of the repository opens
/// `HEAD`, and must not cause another.
fn moves_files(kind: &EventKind) -> bool {
    match kind {
        EventKind::Access(_) | EventKind::Modify(ModifyKind::Metadata(_)) => false,
        EventKind::Any | EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Other => true,
    }
}

/// The repositories a change to `paths` can affect, each once.
fn affected(routes: &[(String, WatchPaths)], paths: &[PathBuf]) -> Vec<String> {
    let mut ids: Vec<String> = routes
        .iter()
        .filter(|(_, watch)| paths.iter().any(|path| watch.affects_head(path)))
        .map(|(id, _)| id.clone())
        .collect();
    ids.dedup();
    ids
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::Duration;

    use notify::event::{AccessKind, CreateKind, DataChange, MetadataKind, RenameMode};

    use super::*;

    const A: &str = "b3c837bd2fae5f34633831c84cd45a856d99a76e";

    fn checkout(at: &Path) -> PathBuf {
        let git = at.join(".git");
        fs::create_dir_all(git.join("refs").join("heads")).expect("mkdir");
        fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
        fs::write(git.join("refs").join("heads").join("main"), A).expect("ref");
        fs::canonicalize(git).expect("canonicalize")
    }

    #[test]
    fn reads_and_metadata_are_not_changes() {
        assert!(!moves_files(&EventKind::Access(AccessKind::Any)));
        assert!(!moves_files(&EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any))));
        assert!(moves_files(&EventKind::Modify(ModifyKind::Name(RenameMode::To))));
        assert!(moves_files(&EventKind::Modify(ModifyKind::Data(DataChange::Any))));
        assert!(moves_files(&EventKind::Create(CreateKind::File)));
    }

    #[test]
    fn a_change_reaches_the_repositories_sharing_its_refs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let git = checkout(&dir.path().join("main"));
        let wt = dir.path().join("wt");
        let own = git.join("worktrees").join("wt");
        fs::create_dir_all(&own).expect("mkdir");
        fs::write(own.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
        fs::write(own.join("commondir"), "../..\n").expect("commondir");
        fs::create_dir_all(&wt).expect("mkdir");
        fs::write(wt.join(".git"), "gitdir: ../main/.git/worktrees/wt\n").expect(".git");
        let routes = vec![
            ("main".to_owned(), watch_paths(&dir.path().join("main")).expect("main")),
            ("wt".to_owned(), watch_paths(&wt).expect("wt")),
        ];

        assert_eq!(affected(&routes, &[git.join("HEAD")]), ["main"]);
        assert_eq!(affected(&routes, &[own.join("HEAD")]), ["wt"]);
        assert_eq!(affected(&routes, &[git.join("refs").join("heads").join("main")]), ["main", "wt"]);
        assert!(affected(&routes, &[git.join("index"), git.join("HEAD.lock")]).is_empty());
    }

    /// Signoff's own worktree of a tracked checkout is not tracked: what
    /// preparing it writes (its HEAD, index and FETCH_HEAD under
    /// `.git/worktrees`) is not the checkout changing.
    #[test]
    fn a_managed_worktree_changing_is_not_the_checkout_changing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let git = checkout(&dir.path().join("main"));
        let own = git.join("worktrees").join("a1b2c3d4e5f60718");
        fs::create_dir_all(&own).expect("mkdir");
        let routes = vec![("main".to_owned(), watch_paths(&dir.path().join("main")).expect("main"))];
        let written: Vec<PathBuf> =
            ["HEAD", "index", "FETCH_HEAD", "ORIG_HEAD", "HEAD.lock"].iter().map(|name| own.join(name)).collect();
        assert!(affected(&routes, &written).is_empty());
        assert!(affected(&routes, &[git.join("worktrees"), own.clone()]).is_empty());
        assert_eq!(affected(&routes, &[git.join("HEAD")]), ["main"], "the checkout itself still counts");
    }

    #[test]
    fn watching_follows_the_tracked_repositories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let one = checkout(&dir.path().join("one"));
        let two = checkout(&dir.path().join("two"));
        let heads = |git: &Path| (git.join("refs").join("heads"), true);
        let mut watcher = GitWatcher::new(|_| {});
        if watcher.watcher.is_none() {
            return; // No watcher on this machine; nothing else to check.
        }

        watcher.watch([("1".to_owned(), dir.path().join("one")), ("2".to_owned(), dir.path().join("two"))]);
        let mut both = vec![(one.clone(), false), heads(&one), (two.clone(), false), heads(&two)];
        both.sort();
        assert_eq!(watcher.watched(), both);

        watcher.watch([("2".to_owned(), dir.path().join("two")), ("3".to_owned(), dir.path().join("not-a-checkout"))]);
        assert_eq!(watcher.watched(), [(two.clone(), false), heads(&two)], "one dropped, a folder skipped");

        watcher.watch([]);
        assert!(watcher.watched().is_empty());
    }

    /// A real checkout of a new branch reaches the callback. Watchers on CI
    /// machines can be slow or missing, so this runs only on request:
    /// `cargo test -p signoff -- --ignored watcher`.
    #[test]
    #[ignore = "depends on the machine's file watcher"]
    fn checking_out_a_branch_is_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let git = checkout(dir.path());
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let mut watcher = GitWatcher::new(move |ids| {
            let _ = tx.lock().map(|tx| tx.send(ids));
        });
        watcher.watch([("repo".to_owned(), dir.path().to_owned())]);
        std::thread::sleep(Duration::from_millis(200));
        fs::write(git.join("HEAD.lock"), "ref: refs/heads/topic\n").expect("lock");
        fs::rename(git.join("HEAD.lock"), git.join("HEAD")).expect("rename");
        let ids = rx.recv_timeout(Duration::from_secs(10)).expect("an event");
        assert_eq!(ids, ["repo"]);
    }
}
