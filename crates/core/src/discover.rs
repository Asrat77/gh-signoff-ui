//! Finds GitHub checkouts already on this computer.
//!
//! A bounded walk of the usual code folders under the home directory,
//! two levels deep. A folder counts when it has a `.git` directory (or a
//! `.git` file, as worktrees and submodules do) whose config lists a
//! github.com remote. Nothing is spawned: the remote comes straight from
//! the git config, so the walk is cheap enough to run on any thread.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Folders under the home directory that people keep checkouts in.
pub const COMMON_FOLDERS: &[&str] = &[
    "code",
    "src",
    "dev",
    "Projects",
    "projects",
    "GitHub",
    "github",
    "workspace",
    "repos",
    "Developer",
    "Documents/GitHub",
];

/// How many levels below a common folder a checkout may sit.
const MAX_DEPTH: usize = 2;
/// Directories read in one scan, so a huge tree cannot stall it.
const MAX_DIRS: usize = 2000;
/// Checkouts reported in one scan.
const MAX_RESULTS: usize = 100;
/// Folders that never hold a checkout worth offering.
const SKIPPED: &[&str] = &["node_modules", "target", "vendor"];

/// A checkout with a GitHub remote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The checkout's canonical path.
    pub path: PathBuf,
    /// `owner/name` of its GitHub remote.
    pub name_with_owner: String,
}

/// Scans the common folders under `home`, leaving out the checkouts at
/// `tracked`. Results are sorted by name.
pub fn scan_home(home: &Path, tracked: &[PathBuf]) -> Vec<Found> {
    let roots: Vec<PathBuf> = COMMON_FOLDERS.iter().map(|folder| home.join(folder)).collect();
    scan(&roots, tracked)
}

/// Scans `roots` for checkouts, leaving out the ones at `tracked`.
pub fn scan(roots: &[PathBuf], tracked: &[PathBuf]) -> Vec<Found> {
    let skip: HashSet<PathBuf> = tracked.iter().filter_map(|path| fs::canonicalize(path).ok()).collect();
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    let mut visited = 0;
    for root in roots {
        let Ok(root) = fs::canonicalize(root) else { continue };
        if !seen.insert(root.clone()) {
            continue;
        }
        walk(&root, 1, &skip, &mut seen, &mut visited, &mut found);
        if visited >= MAX_DIRS || found.len() >= MAX_RESULTS {
            break;
        }
    }
    found.sort_by(|a, b| (a.name_with_owner.to_lowercase(), &a.path).cmp(&(b.name_with_owner.to_lowercase(), &b.path)));
    found
}

fn walk(
    dir: &Path,
    depth: usize,
    skip: &HashSet<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    visited: &mut usize,
    found: &mut Vec<Found>,
) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut children: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            !name.starts_with('.') && !SKIPPED.contains(&name.as_ref())
        })
        .map(|entry| entry.path())
        .collect();
    children.sort();
    for child in children {
        if *visited >= MAX_DIRS || found.len() >= MAX_RESULTS {
            return;
        }
        *visited += 1;
        let Ok(canonical) = fs::canonicalize(&child) else { continue };
        if !seen.insert(canonical.clone()) {
            continue;
        }
        if child.join(".git").exists() {
            // A checkout: offer it, and do not look inside it.
            if !skip.contains(&canonical)
                && let Some(name_with_owner) = github_name(&child)
            {
                found.push(Found { path: canonical, name_with_owner });
            }
        } else if depth < MAX_DEPTH {
            walk(&child, depth + 1, skip, seen, visited, found);
        }
    }
}

/// `owner/name` of the checkout's GitHub remote, if it has one. `origin`
/// wins over other remotes.
pub fn github_name(checkout: &Path) -> Option<String> {
    let config = fs::read_to_string(git_dir(checkout)?.join("config")).ok()?;
    let remotes = remotes(&config);
    remotes
        .iter()
        .find(|(name, _)| name == "origin")
        .and_then(|(_, url)| parse_github_remote(url))
        .or_else(|| remotes.iter().find_map(|(_, url)| parse_github_remote(url)))
}

/// The directory holding the repository's config: `.git` itself, or for a
/// worktree the common directory its `gitdir:` file leads to.
fn git_dir(checkout: &Path) -> Option<PathBuf> {
    let dot_git = checkout.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let text = fs::read_to_string(&dot_git).ok()?;
    let target = text.lines().find_map(|line| line.trim().strip_prefix("gitdir:"))?.trim();
    let gitdir = checkout.join(target);
    // A worktree's own directory points at the repository's with `commondir`.
    if let Ok(common) = fs::read_to_string(gitdir.join("commondir")) {
        let common = gitdir.join(common.trim());
        if common.join("config").is_file() {
            return Some(common);
        }
    }
    gitdir.join("config").is_file().then_some(gitdir)
}

/// Every `[remote "name"]` and its `url`, in file order.
fn remotes(config: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    for line in config.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[') {
            current = header
                .strip_suffix(']')
                .and_then(|h| h.trim().strip_prefix("remote"))
                .and_then(|h| h.trim().strip_prefix('"')?.strip_suffix('"'))
                .map(str::to_owned);
        } else if let Some(name) = &current
            && let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("url")
        {
            out.push((name.clone(), value.trim().to_owned()));
        }
    }
    out
}

/// `owner/name` from a github.com remote URL in any of its forms
/// (`https://github.com/o/n.git`, `git@github.com:o/n.git`,
/// `ssh://git@github.com/o/n`), or None for other hosts.
pub fn parse_github_remote(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    // Drop credentials, then split the host from the path at `/` or `:`.
    let rest = rest.rsplit_once('@').map_or(rest, |(_, after)| after);
    let (host, path) = rest.split_once(['/', ':'])?;
    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    let valid = |part: &str| !part.is_empty() && !part.contains(['/', ' ']);
    (valid(owner) && valid(name)).then(|| format!("{owner}/{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkout(dir: &Path, url: &str) {
        fs::create_dir_all(dir.join(".git")).expect("git dir");
        fs::write(dir.join(".git/config"), format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {url}\n"))
            .expect("config");
    }

    fn names(found: &[Found]) -> Vec<&str> {
        found.iter().map(|f| f.name_with_owner.as_str()).collect()
    }

    #[test]
    fn parses_every_github_form() {
        for url in [
            "https://github.com/acme/widgets.git",
            "https://github.com/acme/widgets",
            "https://github.com/acme/widgets/",
            "http://github.com/acme/widgets.git",
            "git@github.com:acme/widgets.git",
            "ssh://git@github.com/acme/widgets.git",
            "https://user:token@github.com/acme/widgets.git",
            "git://github.com/acme/widgets.git",
        ] {
            assert_eq!(parse_github_remote(url).as_deref(), Some("acme/widgets"), "{url}");
        }
    }

    #[test]
    fn rejects_other_hosts_and_malformed_urls() {
        assert_eq!(parse_github_remote("https://gitlab.com/acme/widgets.git"), None);
        assert_eq!(parse_github_remote("git@bitbucket.org:acme/widgets.git"), None);
        assert_eq!(parse_github_remote("https://notgithub.com/acme/widgets"), None);
        assert_eq!(parse_github_remote("https://github.com/acme"), None);
        assert_eq!(parse_github_remote("/srv/git/widgets.git"), None);
        assert_eq!(parse_github_remote(""), None);
    }

    #[test]
    fn finds_checkouts_two_levels_deep_and_no_deeper() {
        let home = tempfile::tempdir().expect("home");
        checkout(&home.path().join("code/widgets"), "git@github.com:acme/widgets.git");
        checkout(&home.path().join("Documents/GitHub/tools"), "https://github.com/acme/tools");
        checkout(&home.path().join("src/work/api"), "https://github.com/acme/api.git");
        checkout(&home.path().join("src/a/b/too-deep"), "https://github.com/acme/deep.git");
        checkout(&home.path().join("code/elsewhere"), "https://gitlab.com/acme/elsewhere.git");
        fs::create_dir_all(home.path().join("code/plain")).expect("plain");

        let found = scan_home(home.path(), &[]);
        assert_eq!(names(&found), ["acme/api", "acme/tools", "acme/widgets"]);
        assert_eq!(found[2].path, fs::canonicalize(home.path().join("code/widgets")).expect("canonical"));
    }

    #[test]
    fn skips_hidden_and_vendored_folders() {
        let home = tempfile::tempdir().expect("home");
        checkout(&home.path().join("code/.cache/hidden"), "https://github.com/acme/hidden");
        checkout(&home.path().join("code/node_modules/dep"), "https://github.com/acme/dep");
        checkout(&home.path().join("code/target/x"), "https://github.com/acme/x");
        checkout(&home.path().join("code/vendor/y"), "https://github.com/acme/y");
        checkout(&home.path().join("code/real"), "https://github.com/acme/real");
        assert_eq!(names(&scan_home(home.path(), &[])), ["acme/real"]);
    }

    #[test]
    fn leaves_out_tracked_checkouts_however_they_are_spelled() {
        let home = tempfile::tempdir().expect("home");
        checkout(&home.path().join("code/a"), "https://github.com/acme/a");
        checkout(&home.path().join("code/b"), "https://github.com/acme/b");
        let spelled = home.path().join("code/../code/a/");
        let found = scan_home(home.path(), &[spelled, home.path().join("missing")]);
        assert_eq!(names(&found), ["acme/b"]);
    }

    #[test]
    fn prefers_origin_and_falls_back_to_any_github_remote() {
        let home = tempfile::tempdir().expect("home");
        let dir = home.path().join("code/fork");
        fs::create_dir_all(dir.join(".git")).expect("git");
        fs::write(
            dir.join(".git/config"),
            "[remote \"upstream\"]\n\turl = https://github.com/acme/upstream.git\n\
             [remote \"origin\"]\n\turl = https://github.com/me/fork.git\n",
        )
        .expect("config");
        assert_eq!(github_name(&dir).as_deref(), Some("me/fork"));

        let only = home.path().join("code/only");
        fs::create_dir_all(only.join(".git")).expect("git");
        fs::write(
            only.join(".git/config"),
            "[remote \"origin\"]\n\turl = git@gitlab.com:a/b.git\n[remote \"mirror\"]\n\turl = git@github.com:a/b.git\n",
        )
        .expect("config");
        assert_eq!(github_name(&only).as_deref(), Some("a/b"));
    }

    #[test]
    fn follows_a_worktree_to_its_repository_config() {
        let home = tempfile::tempdir().expect("home");
        let main = home.path().join("code/main");
        checkout(&main, "https://github.com/acme/main.git");
        let admin = main.join(".git/worktrees/feature");
        fs::create_dir_all(&admin).expect("admin");
        fs::write(admin.join("commondir"), "../..\n").expect("commondir");
        let tree = home.path().join("code/feature");
        fs::create_dir_all(&tree).expect("tree");
        fs::write(tree.join(".git"), format!("gitdir: {}\n", admin.display())).expect("dot git");

        assert_eq!(names(&scan_home(home.path(), &[])), ["acme/main", "acme/main"]);
    }

    #[test]
    fn caps_the_results() {
        let home = tempfile::tempdir().expect("home");
        for i in 0..(MAX_RESULTS + 20) {
            checkout(&home.path().join(format!("code/r{i:03}")), &format!("https://github.com/acme/r{i}"));
        }
        assert_eq!(scan_home(home.path(), &[]).len(), MAX_RESULTS);
    }

    #[test]
    fn a_missing_home_finds_nothing() {
        assert!(scan_home(Path::new("/definitely/not/here"), &[]).is_empty());
    }
}
