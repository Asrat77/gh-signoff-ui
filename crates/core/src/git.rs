//! The branch and commit checked out in a local checkout, read from the
//! files under `.git` instead of spawning `git` twice per read.
//!
//! Only the common layouts are read: a `.git` directory, a `.git` file
//! pointing at a linked worktree's git directory (with its `commondir`),
//! loose refs, and `packed-refs`. Anything else (the reftable backend, an
//! unborn branch, a `HEAD` pointing outside `refs/heads/`, a ref name that
//! looks wrong) returns `None`, and the caller asks `git` as before. So
//! this can be wrong only by being unsure, never by guessing.

use std::fs;
use std::path::{Path, PathBuf};

/// What is checked out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LocalHead {
    /// The branch, as `git rev-parse --abbrev-ref HEAD` prints it: `HEAD`
    /// when detached.
    pub(crate) branch: String,
    /// The commit.
    pub(crate) sha: String,
}

/// Symbolic refs followed before giving up, as git does.
const MAX_SYMREF_DEPTH: usize = 5;

/// The branch and commit checked out at `dir` (or the checkout `dir` is
/// inside), or `None` when the files do not say for certain.
pub(crate) fn read_head(dir: &Path) -> Option<LocalHead> {
    let git_dir = find_git_dir(dir)?;
    let common = common_dir(&git_dir);
    if common.join("reftable").is_dir() {
        return None;
    }
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(target) => {
            let target = target.trim();
            let branch = target.strip_prefix("refs/heads/")?;
            let sha = resolve(&git_dir, &common, target, 0)?;
            Some(LocalHead { branch: branch.to_owned(), sha })
        }
        None => is_object_id(head).then(|| LocalHead { branch: "HEAD".into(), sha: head.to_owned() }),
    }
}

/// The files that say what a checkout has checked out, for a watcher: a
/// change to any of them can move [`read_head`]'s answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchPaths {
    /// The checkout's own git directory, which holds `HEAD`. Watched on its
    /// own, not its subfolders: objects and the index churn constantly.
    pub git_dir: PathBuf,
    /// Where shared refs and `packed-refs` live: a linked worktree's
    /// `commondir`, otherwise the same as [`Self::git_dir`].
    pub common_dir: PathBuf,
}

impl WatchPaths {
    /// What to watch, each with whether its subfolders are watched too.
    /// Folders rather than files: git replaces `HEAD` and refs by renaming
    /// a lock file over them, which a watch on the file itself would lose.
    pub fn targets(&self) -> Vec<(PathBuf, bool)> {
        let mut targets = vec![(self.git_dir.clone(), false)];
        if self.common_dir != self.git_dir {
            targets.push((self.common_dir.clone(), false));
        }
        for refs in [self.common_dir.join("refs").join("heads"), self.common_dir.join("reftable")] {
            if refs.is_dir() {
                targets.push((refs, true));
            }
        }
        targets
    }

    /// Whether a change at `path` can move what is checked out: `HEAD`,
    /// `packed-refs`, a branch, or the reftable. Lock files are not: the
    /// rename that ends a write is.
    pub fn affects_head(&self, path: &Path) -> bool {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else { return false };
        if name.ends_with(".lock") {
            return false;
        }
        let parent = path.parent();
        (name == "HEAD" && parent == Some(&self.git_dir))
            || (name == "packed-refs" && parent == Some(&self.common_dir))
            || path.starts_with(self.common_dir.join("refs").join("heads"))
            || path.starts_with(self.common_dir.join("reftable"))
    }
}

/// What to watch for the checkout at or above `dir`, with real paths
/// (watchers report those, `/private/var` for `/var` on macOS). `None`
/// when it is not a checkout.
pub fn watch_paths(dir: &Path) -> Option<WatchPaths> {
    let real = |path: PathBuf| fs::canonicalize(&path).unwrap_or(path);
    let git_dir = real(find_git_dir(dir)?);
    if !git_dir.is_dir() {
        return None;
    }
    let common_dir = real(common_dir(&git_dir));
    Some(WatchPaths { git_dir, common_dir })
}

/// The git directory for the checkout at or above `dir`.
pub(crate) fn find_git_dir(dir: &Path) -> Option<PathBuf> {
    for candidate in dir.ancestors() {
        let dot_git = candidate.join(".git");
        if dot_git.is_dir() {
            return Some(dot_git);
        }
        if dot_git.is_file() {
            // A linked worktree or submodule: `gitdir: <path>`, relative to
            // the checkout when not absolute.
            let text = fs::read_to_string(&dot_git).ok()?;
            let target = text.trim().strip_prefix("gitdir:")?.trim();
            return Some(candidate.join(target));
        }
    }
    None
}

/// Where shared refs live: a linked worktree's `commondir`, or the git
/// directory itself.
pub(crate) fn common_dir(git_dir: &Path) -> PathBuf {
    match fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) if !text.trim().is_empty() => git_dir.join(text.trim()),
        _ => git_dir.to_owned(),
    }
}

/// The commit `name` (`refs/heads/main`) points at.
fn resolve(git_dir: &Path, common: &Path, name: &str, depth: usize) -> Option<String> {
    if depth > MAX_SYMREF_DEPTH || !safe_ref_name(name) {
        return None;
    }
    // Per-worktree refs first, then shared ones.
    for base in [git_dir, common] {
        if let Ok(text) = fs::read_to_string(base.join(name)) {
            let text = text.trim();
            if let Some(target) = text.strip_prefix("ref:") {
                return resolve(git_dir, common, target.trim(), depth + 1);
            }
            return is_object_id(text).then(|| text.to_owned());
        }
    }
    packed(common, name)
}

/// `name` in `packed-refs`.
fn packed(common: &Path, name: &str) -> Option<String> {
    let text = fs::read_to_string(common.join("packed-refs")).ok()?;
    text.lines()
        .filter(|line| !line.starts_with('#') && !line.starts_with('^'))
        .filter_map(|line| line.split_once(' '))
        .find(|(_, refname)| refname.trim() == name)
        .map(|(sha, _)| sha.to_owned())
        .filter(|sha| is_object_id(sha))
}

/// A ref name that stays inside the git directory.
fn safe_ref_name(name: &str) -> bool {
    name.starts_with("refs/") && !name.split('/').any(|part| part.is_empty() || part == "." || part == "..")
}

/// A full SHA-1 or SHA-256 object id.
fn is_object_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "b3c837bd2fae5f34633831c84cd45a856d99a76e";
    const B: &str = "3db401c9e52c1a0b8d6e7f00112233445566778f";

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, text).expect("write");
    }

    #[test]
    fn reads_a_branch_from_a_loose_ref() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(&dir.path().join(".git/HEAD"), "ref: refs/heads/feature/x\n");
        write(&dir.path().join(".git/refs/heads/feature/x"), &format!("{A}\n"));
        let head = read_head(dir.path()).expect("head");
        assert_eq!(head, LocalHead { branch: "feature/x".into(), sha: A.into() });
    }

    #[test]
    fn reads_a_branch_from_packed_refs_and_from_a_subdirectory() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(&dir.path().join(".git/HEAD"), "ref: refs/heads/main\n");
        write(
            &dir.path().join(".git/packed-refs"),
            &format!("# pack-refs with: peeled fully-peeled sorted\n{B} refs/heads/other\n{A} refs/heads/main\n^{B}\n"),
        );
        let sub = dir.path().join("crates/core");
        fs::create_dir_all(&sub).expect("mkdir");
        assert_eq!(read_head(&sub).expect("head"), LocalHead { branch: "main".into(), sha: A.into() });
    }

    #[test]
    fn a_loose_ref_wins_over_a_packed_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(&dir.path().join(".git/HEAD"), "ref: refs/heads/main\n");
        write(&dir.path().join(".git/packed-refs"), &format!("{A} refs/heads/main\n"));
        write(&dir.path().join(".git/refs/heads/main"), B);
        assert_eq!(read_head(dir.path()).expect("head").sha, B);
    }

    #[test]
    fn a_detached_head_reports_head() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(&dir.path().join(".git/HEAD"), &format!("{A}\n"));
        assert_eq!(read_head(dir.path()).expect("head"), LocalHead { branch: "HEAD".into(), sha: A.into() });
    }

    #[test]
    fn a_linked_worktree_reads_its_own_head_and_the_shared_refs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let main = dir.path().join("main");
        let wt = dir.path().join("wt");
        write(&main.join(".git/HEAD"), "ref: refs/heads/main\n");
        write(&main.join(".git/refs/heads/main"), A);
        write(&main.join(".git/refs/heads/pr-7"), B);
        write(&main.join(".git/worktrees/wt/HEAD"), "ref: refs/heads/pr-7\n");
        write(&main.join(".git/worktrees/wt/commondir"), "../..\n");
        write(&wt.join(".git"), "gitdir: ../main/.git/worktrees/wt\n");
        assert_eq!(read_head(&wt).expect("head"), LocalHead { branch: "pr-7".into(), sha: B.into() });
    }

    #[test]
    fn unsure_answers_are_left_to_git() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(read_head(dir.path()), None, "not a checkout");

        write(&dir.path().join(".git/HEAD"), "ref: refs/heads/unborn\n");
        assert_eq!(read_head(dir.path()), None, "an unborn branch");

        write(&dir.path().join(".git/HEAD"), "ref: refs/heads/../../config\n");
        assert_eq!(read_head(dir.path()), None, "a ref outside .git/refs");

        write(&dir.path().join(".git/HEAD"), "ref: refs/heads/.invalid\n");
        fs::create_dir_all(dir.path().join(".git/reftable")).expect("mkdir");
        assert_eq!(read_head(dir.path()), None, "the reftable backend");
    }

    fn real(path: &Path) -> PathBuf {
        fs::canonicalize(path).expect("canonicalize")
    }

    /// `base` joined with `rel` one part at a time, as a watcher reports
    /// it: a verbatim Windows path does not split on `/`.
    fn under(base: &Path, rel: &str) -> PathBuf {
        rel.split('/').fold(base.to_owned(), |path, part| path.join(part))
    }

    #[test]
    fn a_checkout_watches_its_git_directory_and_branches_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(&dir.path().join(".git/HEAD"), "ref: refs/heads/main\n");
        write(&dir.path().join(".git/refs/heads/main"), A);
        fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        let git = real(&dir.path().join(".git"));

        let paths = watch_paths(&dir.path().join("src")).expect("a checkout");
        assert_eq!(paths, WatchPaths { git_dir: git.clone(), common_dir: git.clone() });
        assert_eq!(paths.targets(), [(git.clone(), false), (git.join("refs").join("heads"), true)]);

        assert!(paths.affects_head(&git.join("HEAD")));
        assert!(paths.affects_head(&git.join("packed-refs")));
        assert!(paths.affects_head(&under(&git, "refs/heads/feature/x")));
        assert!(!paths.affects_head(&git.join("HEAD.lock")), "the rename that follows counts");
        assert!(!paths.affects_head(&under(&git, "refs/heads/main.lock")));
        assert!(!paths.affects_head(&git.join("index")), "staging changes nothing checked out");
        assert!(!paths.affects_head(&git.join("FETCH_HEAD")));
        assert!(!paths.affects_head(&under(&git, "refs/remotes/origin/main")));
        assert!(!paths.affects_head(&real(dir.path()).join("HEAD")), "a file in the working tree");
    }

    #[test]
    fn a_linked_worktree_watches_its_own_head_and_the_shared_refs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let main = dir.path().join("main");
        let wt = dir.path().join("wt");
        write(&main.join(".git/HEAD"), "ref: refs/heads/main\n");
        write(&main.join(".git/refs/heads/main"), A);
        write(&main.join(".git/worktrees/wt/HEAD"), "ref: refs/heads/main\n");
        write(&main.join(".git/worktrees/wt/commondir"), "../..\n");
        write(&wt.join(".git"), "gitdir: ../main/.git/worktrees/wt\n");
        let common = real(&main.join(".git"));
        let own = common.join("worktrees").join("wt");

        let paths = watch_paths(&wt).expect("a worktree");
        assert_eq!(paths, WatchPaths { git_dir: own.clone(), common_dir: common.clone() });
        assert_eq!(
            paths.targets(),
            [(own.clone(), false), (common.clone(), false), (common.join("refs").join("heads"), true)]
        );
        assert!(paths.affects_head(&own.join("HEAD")));
        assert!(!paths.affects_head(&common.join("HEAD")), "the main checkout's HEAD is not ours");
        assert!(paths.affects_head(&common.join("packed-refs")));
        assert!(paths.affects_head(&under(&common, "refs/heads/main")));
        assert!(!paths.affects_head(&own.join("packed-refs")));
    }

    #[test]
    fn a_reftable_repository_watches_its_tables_and_a_folder_without_git_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(watch_paths(dir.path()), None);
        write(&dir.path().join(".git/HEAD"), "ref: refs/heads/.invalid\n");
        write(&dir.path().join(".git/reftable/tables.list"), "");
        let git = real(&dir.path().join(".git"));
        let paths = watch_paths(dir.path()).expect("a checkout");
        assert_eq!(paths.targets(), [(git.clone(), false), (git.join("reftable"), true)]);
        assert!(paths.affects_head(&under(&git, "reftable/tables.list")));

        let broken = tempfile::tempdir().expect("tempdir");
        write(&broken.path().join(".git"), "gitdir: ../nowhere\n");
        assert_eq!(watch_paths(broken.path()), None, "a .git file pointing at nothing");
    }

    /// The files must agree with `git` itself on a real repository, packed
    /// and loose.
    #[test]
    fn agrees_with_git() {
        let dir = tempfile::tempdir().expect("tempdir");
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git").args(args).current_dir(dir.path()).output().expect("git");
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        let commit = |message: &str| {
            git(&["-c", "user.name=T", "-c", "user.email=t@example.test", "commit", "--allow-empty", "-qm", message]);
        };
        git(&["init", "-q", "-b", "trunk"]);
        commit("one");
        git(&["checkout", "-q", "-b", "topic/a"]);
        commit("two");
        let expect = |label: &str| {
            let want =
                LocalHead { branch: git(&["rev-parse", "--abbrev-ref", "HEAD"]), sha: git(&["rev-parse", "HEAD"]) };
            assert_eq!(read_head(dir.path()).as_ref(), Some(&want), "{label}");
        };
        expect("loose");
        git(&["pack-refs", "--all"]);
        expect("packed");
        git(&["checkout", "-q", "--detach", "trunk"]);
        expect("detached");
    }
}
