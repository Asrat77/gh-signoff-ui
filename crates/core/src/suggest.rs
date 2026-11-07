//! Commands worth mapping to a check, read from the checkout.
//!
//! Detection looks only at well-known files at the repository's root and
//! never runs anything. [`rank`] puts the suggestions that match a check's
//! name first, so the check `lint` leads with `npm run lint`.

use std::collections::HashSet;
use std::path::Path;

use crate::files::Tree;

/// The most suggestions [`detect`] returns.
const MAX: usize = 40;

/// One command that could back a check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSuggestion {
    /// The command line to run in the checkout.
    pub command: String,
    /// The target, script or tool it runs: `lint`, `test`, `clippy`.
    pub name: String,
    /// The file it was found in: `Makefile`, `package.json`.
    pub source: &'static str,
}

impl CommandSuggestion {
    fn new(command: impl Into<String>, name: impl Into<String>, source: &'static str) -> Self {
        Self { command: command.into(), name: name.into(), source }
    }
}

/// The commands the checkout at `dir` suggests, in the order they were
/// found.
pub fn detect(dir: &Path) -> Vec<CommandSuggestion> {
    detect_in(dir)
}

/// The commands the files of `tree` suggest, in the order they were found.
pub fn detect_in(tree: &(impl Tree + ?Sized)) -> Vec<CommandSuggestion> {
    let read = |name: &str| tree.read(name);
    let has = |name: &str| tree.has_file(name);
    let mut found = Vec::new();

    if let Some(text) = read("Makefile").or_else(|| read("makefile")).or_else(|| read("GNUmakefile")) {
        for target in make_targets(&text) {
            found.push(CommandSuggestion::new(format!("make {target}"), target, "Makefile"));
        }
    }
    if let Some(text) = read("package.json") {
        let manager = package_manager(tree);
        for script in package_scripts(&text) {
            found.push(CommandSuggestion::new(format!("{manager} run {script}"), script, "package.json"));
        }
    }
    if has("Cargo.toml") {
        found.push(CommandSuggestion::new("cargo test", "test", "Cargo.toml"));
        found.push(CommandSuggestion::new("cargo clippy", "clippy", "Cargo.toml"));
    }
    if has("go.mod") {
        found.push(CommandSuggestion::new("go test ./...", "test", "go.mod"));
        found.push(CommandSuggestion::new("go vet ./...", "vet", "go.mod"));
    }
    if has("pytest.ini") || read("pyproject.toml").is_some_and(|text| text.contains("pytest")) {
        found.push(CommandSuggestion::new("pytest", "test", "pyproject.toml"));
    }
    if let Some(text) = read("justfile").or_else(|| read("Justfile")).or_else(|| read(".justfile")) {
        for recipe in just_recipes(&text) {
            found.push(CommandSuggestion::new(format!("just {recipe}"), recipe, "justfile"));
        }
    }

    let mut seen = HashSet::new();
    found.retain(|s| seen.insert(s.command.clone()));
    found.truncate(MAX);
    found
}

/// `suggestions` ordered for the check `check`: exact name matches first,
/// then related names (`tests` and `test`, `lint` and `clippy`), then names
/// that contain it, then the rest. Detection order is kept within a group.
pub fn rank<'a>(suggestions: &'a [CommandSuggestion], check: &str) -> Vec<&'a CommandSuggestion> {
    let mut ranked: Vec<&CommandSuggestion> = suggestions.iter().collect();
    ranked.sort_by_key(|s| closeness(&s.name, check));
    ranked
}

/// [`detect`] then [`rank`], owned.
pub fn suggest(dir: &Path, check: &str) -> Vec<CommandSuggestion> {
    let all = detect(dir);
    rank(&all, check).into_iter().cloned().collect()
}

/// 0 for the same name, 1 for a synonym, 2 for a substring, 3 otherwise.
pub(crate) fn closeness(name: &str, check: &str) -> u8 {
    let (name, check) = (name.to_ascii_lowercase(), check.to_ascii_lowercase());
    if name == check {
        return 0;
    }
    if let (Some(a), Some(b)) = (family(&name), family(&check))
        && a == b
    {
        return 1;
    }
    if !check.is_empty() && (name.contains(&check) || check.contains(&name)) {
        return 2;
    }
    3
}

/// The group a name belongs to, so `tests` finds `test` and `lint` finds
/// `clippy`.
fn family(name: &str) -> Option<&'static str> {
    let words: Vec<&str> = name.split(|c: char| !c.is_ascii_alphanumeric()).collect();
    let any = |names: &[&str]| words.iter().any(|w| names.contains(w));
    if any(&["test", "tests", "unit", "spec", "pytest"]) {
        Some("test")
    } else if any(&["lint", "linter", "clippy", "eslint", "vet", "ruff", "rubocop", "standardrb", "check"]) {
        Some("lint")
    } else if any(&["fmt", "format", "prettier", "style"]) {
        Some("format")
    } else if any(&["build", "compile", "bundle"]) {
        Some("build")
    } else if any(&["typecheck", "types", "tsc", "mypy"]) {
        Some("types")
    } else if any(&["security", "audit", "brakeman", "vulnerabilities"]) {
        Some("security")
    } else {
        None
    }
}

/// Targets a Makefile defines, without pattern, special and variable lines.
fn make_targets(text: &str) -> Vec<String> {
    let mut targets: Vec<String> = Vec::new();
    for line in text.lines() {
        // Recipes are indented; comments are not targets.
        if line.starts_with(['\t', ' ', '#']) {
            continue;
        }
        let Some((head, rest)) = line.split_once(':') else { continue };
        // `NAME := value` and `NAME = a:b` assign, they do not define.
        if rest.starts_with('=') || head.contains('=') {
            continue;
        }
        for target in head.split_whitespace() {
            let plain = target.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
            if plain && !target.starts_with('.') && !targets.iter().any(|t| t == target) {
                targets.push(target.to_owned());
            }
        }
    }
    targets
}

/// Script names from a `package.json`, in file order, without the
/// `pre`/`post` hooks npm runs on its own.
fn package_scripts(text: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else { return Vec::new() };
    let Some(scripts) = value.get("scripts").and_then(|scripts| scripts.as_object()) else { return Vec::new() };
    let is_hook = |name: &str| {
        let hooked = name.strip_prefix("pre").or_else(|| name.strip_prefix("post"));
        hooked.is_some_and(|rest| scripts.contains_key(rest))
    };
    scripts.keys().filter(|name| !is_hook(name)).cloned().collect()
}

/// The package manager the lockfile names, else npm.
fn package_manager(tree: &(impl Tree + ?Sized)) -> &'static str {
    let has = |name: &str| tree.has_file(name);
    if has("pnpm-lock.yaml") {
        "pnpm"
    } else if has("yarn.lock") {
        "yarn"
    } else if has("bun.lockb") || has("bun.lock") {
        "bun"
    } else {
        "npm"
    }
}

/// Public recipe names from a justfile.
fn just_recipes(text: &str) -> Vec<String> {
    let mut recipes: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.starts_with([' ', '\t', '#']) || line.contains(":=") {
            continue;
        }
        let Some((head, _)) = line.split_once(':') else { continue };
        let name = head.split_whitespace().next().unwrap_or("").trim_start_matches('@');
        let plain = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
        if plain && !name.starts_with('_') && !recipes.iter().any(|r| r == name) {
            recipes.push(name.to_owned());
        }
    }
    recipes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for (name, text) in files {
            fs::write(dir.path().join(name), text).expect("write");
        }
        dir
    }

    fn commands(found: &[CommandSuggestion]) -> Vec<&str> {
        found.iter().map(|s| s.command.as_str()).collect()
    }

    #[test]
    fn makefile_targets_skip_variables_specials_and_recipes() {
        let dir = dir_with(&[(
            "Makefile",
            "CC := gcc\nVERSION=1\n.PHONY: test lint\n\ntest: build\n\tcargo test\nlint:\n\t@echo lint\n%.o: %.c\n\tcc\nbuild test:\n",
        )]);
        assert_eq!(commands(&detect(dir.path())), ["make test", "make lint", "make build"]);
    }

    #[test]
    fn package_scripts_use_the_lockfiles_manager() {
        let json = r#"{"scripts":{"build":"tsc","lint":"eslint .","pretest":"x","test":"jest"}}"#;
        for (lock, manager) in
            [("", "npm"), ("pnpm-lock.yaml", "pnpm"), ("yarn.lock", "yarn"), ("bun.lockb", "bun"), ("bun.lock", "bun")]
        {
            let dir = dir_with(&[("package.json", json)]);
            if !lock.is_empty() {
                fs::write(dir.path().join(lock), "").expect("lock");
            }
            let expected =
                [format!("{manager} run build"), format!("{manager} run lint"), format!("{manager} run test")];
            let found = detect(dir.path());
            assert_eq!(commands(&found), expected, "{lock}");
        }
    }

    #[test]
    fn language_files_suggest_their_standard_commands() {
        let dir = dir_with(&[
            ("Cargo.toml", "[package]"),
            ("go.mod", "module x"),
            ("pyproject.toml", "[tool.pytest.ini_options]"),
            ("justfile", "set shell := [\"sh\"]\ndefault:\n  just --list\n_private:\nci: test\n"),
        ]);
        assert_eq!(
            commands(&detect(dir.path())),
            ["cargo test", "cargo clippy", "go test ./...", "go vet ./...", "pytest", "just default", "just ci"]
        );
    }

    #[test]
    fn a_pyproject_without_pytest_suggests_nothing() {
        let dir = dir_with(&[("pyproject.toml", "[project]\nname = \"x\"")]);
        assert!(detect(dir.path()).is_empty());
        assert!(detect(Path::new("/definitely/not/here")).is_empty());
    }

    #[test]
    fn matching_suggestions_come_first() {
        let dir =
            dir_with(&[("package.json", r#"{"scripts":{"build":"b","lint":"l","test":"t"}}"#), ("Cargo.toml", "")]);
        let first = |check: &str| suggest(dir.path(), check).remove(0).command;
        assert_eq!(first("lint"), "npm run lint", "exact before the synonym clippy");
        assert_eq!(first("tests"), "npm run test", "synonym, in detection order");
        assert_eq!(first("build"), "npm run build");
        assert_eq!(first("clippy"), "cargo clippy");
        let all = suggest(dir.path(), "security");
        assert_eq!(all.len(), 5, "nothing is hidden when nothing matches");
        assert_eq!(all[0].command, "npm run build", "detection order is kept");
    }
}
