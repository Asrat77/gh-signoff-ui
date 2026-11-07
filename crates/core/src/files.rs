//! Reading a repository's files, either from the checkout on disk or from
//! a commit, so detection works the same on both.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::gh::Gh;

/// Files of one repository, by `/`-separated path from its root.
pub trait Tree {
    /// The text of the file at `path`, if there is one.
    fn read(&self, path: &str) -> Option<String>;
    /// Whether there is a file at `path`.
    fn has_file(&self, path: &str) -> bool;
    /// The paths of the files directly in `dir`, sorted.
    fn list(&self, dir: &str) -> Vec<String>;
}

/// The checkout as it is on disk.
impl Tree for Path {
    fn read(&self, path: &str) -> Option<String> {
        fs::read_to_string(self.join(path)).ok()
    }

    fn has_file(&self, path: &str) -> bool {
        self.join(path).is_file()
    }

    fn list(&self, dir: &str) -> Vec<String> {
        let Ok(entries) = fs::read_dir(self.join(dir)) else { return Vec::new() };
        let mut files: Vec<String> = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .map(|name| format!("{}/{name}", dir.trim_end_matches('/')))
            .collect();
        files.sort();
        files
    }
}

/// The files of one commit, read with `git` and never checked out, so what
/// a pull request's branch changes cannot leak in.
pub(crate) struct GitTree<'a> {
    gh: &'a Gh,
    dir: &'a Path,
    commit: String,
    /// Every path [`GitTree::WATCHED`] covers, listed once up front so a
    /// missing file costs no `git` call.
    files: BTreeSet<String>,
}

impl<'a> GitTree<'a> {
    /// Where detection looks: the root, the workflows and Rails' CI steps.
    const WATCHED: [&'static str; 3] = [".", ".github/workflows/", "config/ci.rb"];

    /// The files of `commit` in the checkout at `dir`.
    pub(crate) fn new(gh: &'a Gh, dir: &'a Path, commit: &str) -> Self {
        let files = gh.tree_files(dir, commit, &Self::WATCHED).into_iter().collect();
        Self { gh, dir, commit: commit.to_owned(), files }
    }
}

impl Tree for GitTree<'_> {
    fn read(&self, path: &str) -> Option<String> {
        if !self.has_file(path) {
            return None;
        }
        self.gh.show_file(self.dir, &self.commit, path)
    }

    fn has_file(&self, path: &str) -> bool {
        self.files.contains(path)
    }

    fn list(&self, dir: &str) -> Vec<String> {
        let prefix = format!("{}/", dir.trim_end_matches('/'));
        self.files
            .iter()
            .filter(|path| path.strip_prefix(&prefix).is_some_and(|rest| !rest.is_empty() && !rest.contains('/')))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::Arc;

    use crate::runner::SystemRunner;

    fn write(dir: &Path, path: &str, text: &str) {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, text).expect("write");
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    #[test]
    fn a_folder_lists_only_its_own_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), ".github/workflows/ci.yml", "a");
        write(dir.path(), ".github/workflows/nested/x.yml", "b");
        let tree = dir.path();
        assert_eq!(tree.list(".github/workflows"), [".github/workflows/ci.yml"]);
        assert_eq!(tree.read(".github/workflows/ci.yml").as_deref(), Some("a"));
        assert!(tree.has_file(".github/workflows/ci.yml") && !tree.has_file(".github/workflows"));
        assert!(tree.list("missing").is_empty());
    }

    #[test]
    fn a_commit_is_read_without_its_working_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        git(root, &["init", "-q"]);
        write(root, "Makefile", "test:\n");
        write(root, ".github/workflows/ci.yml", "committed");
        write(root, "config/ci.rb", "step \"Tests\", \"bin/rails test\"\n");
        write(root, "src/deep/file.rs", "");
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", "init"]);
        let commit = git(root, &["rev-parse", "HEAD"]);
        // What the working tree has since must not show.
        write(root, ".github/workflows/ci.yml", "edited");
        write(root, ".github/workflows/new.yml", "untracked");

        let gh = Gh::new(Arc::new(SystemRunner::default()));
        let tree = GitTree::new(&gh, root, &commit);
        assert_eq!(tree.list(".github/workflows"), [".github/workflows/ci.yml"]);
        assert_eq!(tree.read(".github/workflows/ci.yml").as_deref(), Some("committed"));
        assert!(tree.has_file("Makefile") && tree.has_file("config/ci.rb"));
        assert!(!tree.has_file("src/deep/file.rs"), "only the watched paths are listed");
        assert_eq!(tree.read("go.mod"), None);
    }
}
