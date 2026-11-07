//! Sets up each signoff check's local command from the repository's own
//! CI config, so Run & sign off works without typing commands first.
//!
//! [`detect`] reads, in order of trust:
//!
//! 1. Rails' `config/ci.rb` (what `bin/ci` runs), whose steps `gh signoff`
//!    was made for;
//! 2. GitHub Actions workflows that run on pushes or pull requests: each
//!    job, and each `run:` step in it;
//! 3. the commands [`crate::suggest`] finds in the `Makefile`,
//!    `package.json` and the like, for close name matches only.
//!
//! A check gets the command whose job or step name matches it best, the
//! way [`suggest::rank`] orders suggestions: `tests` finds the job `test`,
//! `lint` finds the step `Run clippy`. The bare `signoff` check, which
//! vouches for the whole pull request, gets every step chained.
//!
//! Only what can run as is on a developer's computer is kept: installs,
//! `echo`s and other CI setup are dropped, and a step that needs GitHub's
//! `${{ }}` expressions, its environment or shell control flow is skipped.
//! Nothing here runs anything.

use std::collections::BTreeMap;

use yaml_rust2::{Yaml, YamlLoader};

use crate::config::DetectedCommand;
use crate::files::Tree;
use crate::suggest::{self, closeness};

/// Rails' CI steps.
const RAILS_CI: &str = "config/ci.rb";
/// Where GitHub Actions workflows live.
const WORKFLOWS: &str = ".github/workflows";
/// The check that vouches for the whole pull request.
pub const WHOLE: &str = "signoff";

/// Where a candidate came from; earlier kinds win ties.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    RailsStep,
    Job,
    Step,
    RepoFile,
}

/// One command a check could get.
#[derive(Clone, Debug)]
struct Candidate {
    /// What it is called: the job id and name, the step name, the command.
    names: Vec<String>,
    command: String,
    source: String,
    kind: Kind,
}

impl Candidate {
    /// How close its best name is to `check`, as [`closeness`] scores.
    fn closeness(&self, check: &str) -> u8 {
        self.names.iter().map(|name| closeness(name, check)).min().unwrap_or(u8::MAX)
    }
}

/// The commands the CI config in `tree` sets up for `checks`, by check
/// name. Checks it finds nothing for are left out.
pub fn detect<S: AsRef<str>>(tree: &(impl Tree + ?Sized), checks: &[S]) -> BTreeMap<String, DetectedCommand> {
    let rails = tree.read(RAILS_CI).map(|text| rails_steps(&text)).unwrap_or_default();
    let workflows: Vec<Workflow> = tree
        .list(WORKFLOWS)
        .into_iter()
        .filter(|path| path.ends_with(".yml") || path.ends_with(".yaml"))
        .filter_map(|path| tree.read(&path).and_then(|text| workflow(&path, &text)))
        .collect();

    let mut candidates: Vec<Candidate> = rails
        .iter()
        .map(|step| Candidate {
            names: vec![step.title.clone(), step.command.clone()],
            command: step.command.clone(),
            source: RAILS_CI.to_owned(),
            kind: Kind::RailsStep,
        })
        .collect();
    for workflow in &workflows {
        for job in &workflow.jobs {
            if let Some(command) = job.command() {
                candidates.push(Candidate {
                    names: vec![job.id.clone(), job.name.clone()],
                    command,
                    source: workflow.path.clone(),
                    kind: Kind::Job,
                });
            }
            for step in &job.steps {
                candidates.push(Candidate {
                    names: vec![step.name.clone(), step.command.clone()],
                    command: step.command(job.directory.as_deref()),
                    source: workflow.path.clone(),
                    kind: Kind::Step,
                });
            }
        }
    }
    candidates.extend(suggest::detect_in(tree).into_iter().map(|found| Candidate {
        names: vec![found.name],
        command: found.command,
        source: found.source.to_owned(),
        kind: Kind::RepoFile,
    }));

    let mut out = BTreeMap::new();
    for check in checks {
        let check = check.as_ref();
        let found = if check == WHOLE { whole(&rails, &workflows) } else { best(&candidates, check) };
        if let Some(found) = found {
            out.insert(check.to_owned(), found);
        }
    }
    out
}

/// The closest candidate for `check`. The repository's own files only
/// count for a same-name or synonym match, CI for a substring too.
fn best(candidates: &[Candidate], check: &str) -> Option<DetectedCommand> {
    candidates
        .iter()
        .map(|candidate| (candidate.closeness(check), candidate))
        .filter(|(score, candidate)| *score <= if candidate.kind == Kind::RepoFile { 1 } else { 2 })
        .min_by_key(|(score, candidate)| (*score, candidate.kind))
        .map(|(_, candidate)| DetectedCommand { command: candidate.command.clone(), source: candidate.source.clone() })
}

/// Everything CI runs, chained, for the check that vouches for the whole
/// pull request: Rails' steps, else every check-like job's.
fn whole(rails: &[RailsStep], workflows: &[Workflow]) -> Option<DetectedCommand> {
    if !rails.is_empty() {
        let commands: Vec<String> = rails.iter().map(|step| step.command.clone()).collect();
        return Some(DetectedCommand { command: chain(commands), source: RAILS_CI.to_owned() });
    }
    let mut commands = Vec::new();
    let mut sources: Vec<&str> = Vec::new();
    for workflow in workflows {
        for job in workflow.jobs.iter().filter(|job| !job.ships()) {
            let Some(command) = job.command() else { continue };
            if !commands.contains(&command) {
                commands.push(command);
                if !sources.contains(&workflow.path.as_str()) {
                    sources.push(&workflow.path);
                }
            }
        }
    }
    (!commands.is_empty()).then(|| DetectedCommand { command: chain(commands), source: sources.join(", ") })
}

/// The steps of `ci`, a command [`detect`] chained for the whole pull
/// request, that `command` does not run. Empty when it covers them all.
pub fn missing(command: &str, ci: &str) -> Vec<String> {
    let have: Vec<&str> = command.split(" && ").map(str::trim).collect();
    ci.split(" && ").map(str::trim).filter(|step| !step.is_empty() && !have.contains(step)).map(str::to_owned).collect()
}

/// `commands` run one after another, stopping at the first that fails.
/// `&&` means the same in `sh` and `cmd`.
fn chain(commands: Vec<String>) -> String {
    commands.join(" && ")
}

// ---------------------------------------------------------------------------
// Rails' config/ci.rb

/// One `step "Title", "command"` of `config/ci.rb`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RailsStep {
    title: String,
    command: String,
}

/// The steps of `config/ci.rb`, without the one that runs `gh signoff`:
/// Signoff writes the signoff itself, once the rest passed.
fn rails_steps(text: &str) -> Vec<RailsStep> {
    let mut steps = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("step ").or_else(|| line.strip_prefix("step(")) else { continue };
        let Some(mut args) = ruby_strings(rest) else { continue };
        if args.len() < 2 {
            continue;
        }
        let title = args.remove(0);
        let command = if args.len() == 1 {
            args.remove(0)
        } else {
            args.iter().map(|arg| shell_quote(arg)).collect::<Vec<_>>().join(" ")
        };
        if command.trim().is_empty() || runs_signoff(&command) {
            continue;
        }
        steps.push(RailsStep { title, command });
    }
    steps
}

/// The comma-separated string literals at the start of `text`, or `None`
/// when anything else is there (an interpolation, a variable).
fn ruby_strings(text: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut chars = text.trim_start().chars().peekable();
    loop {
        let quote = chars.next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        let mut value = String::new();
        loop {
            match chars.next()? {
                '\\' => value.push(chars.next()?),
                '#' if quote == '"' && chars.peek() == Some(&'{') => return None,
                c if c == quote => break,
                c => value.push(c),
            }
        }
        out.push(value);
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        match chars.next() {
            Some(',') => {
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
            }
            // The end of the call, a trailing `)` or a comment.
            _ => return Some(out),
        }
    }
}

/// `arg` as one shell word.
fn shell_quote(arg: &str) -> String {
    if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c)) {
        arg.to_owned()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

/// Whether `command` writes a signoff itself.
fn runs_signoff(command: &str) -> bool {
    command.split_whitespace().collect::<Vec<_>>().windows(2).any(|w| w == ["gh", "signoff"])
}

// ---------------------------------------------------------------------------
// GitHub Actions

/// A workflow that runs on pushes or pull requests.
#[derive(Debug)]
struct Workflow {
    path: String,
    jobs: Vec<Job>,
}

#[derive(Debug)]
struct Job {
    id: String,
    name: String,
    /// `defaults.run.working-directory`.
    directory: Option<String>,
    /// Its usable `run:` steps.
    steps: Vec<Step>,
}

#[derive(Debug)]
struct Step {
    name: String,
    /// The step's lines, chained.
    command: String,
    /// Its own `working-directory`.
    directory: Option<String>,
}

impl Step {
    /// The step's command, in its working directory or else `fallback`.
    fn command(&self, fallback: Option<&str>) -> String {
        match self.directory.as_deref().or(fallback) {
            Some(dir) if dir != "." => format!("cd {} && {}", shell_quote(dir), self.command),
            _ => self.command.clone(),
        }
    }
}

impl Job {
    /// Every step chained, or `None` without steps, or when steps differ
    /// in working directory (a `cd` would carry over).
    fn command(&self) -> Option<String> {
        let first = self.steps.first()?;
        let directory = first.directory.as_deref().or(self.directory.as_deref());
        if self.steps.iter().any(|step| step.directory.as_deref().or(self.directory.as_deref()) != directory) {
            return None;
        }
        let commands: Vec<String> = self.steps.iter().map(|step| step.command.clone()).collect();
        let chained = chain(commands);
        Some(match directory {
            Some(dir) if dir != "." => format!("cd {} && {chained}", shell_quote(dir)),
            _ => chained,
        })
    }

    /// Whether it ships rather than checks: deploys, releases, packages.
    fn ships(&self) -> bool {
        let words = format!("{} {}", self.id, self.name).to_ascii_lowercase();
        ["deploy", "release", "publish", "package", "docs", "pages"].iter().any(|word| words.contains(word))
    }
}

/// The workflow at `path`, if it parses and runs on pushes or pull requests.
fn workflow(path: &str, text: &str) -> Option<Workflow> {
    let doc = YamlLoader::load_from_str(text).ok()?.into_iter().next()?;
    if !runs_on_changes(&doc["on"]) {
        return None;
    }
    let jobs = doc["jobs"]
        .as_hash()?
        .iter()
        .filter_map(|(id, job)| {
            let id = id.as_str()?.to_owned();
            // A job that calls another workflow has no steps of its own.
            if !job["uses"].is_badvalue() {
                return None;
            }
            let name = job["name"].as_str().map_or_else(|| id.clone(), strip_expressions);
            let directory = job["defaults"]["run"]["working-directory"].as_str().map(str::to_owned);
            let steps = job["steps"].as_vec().map(|steps| steps.iter().filter_map(step).collect()).unwrap_or_default();
            Some(Job { id, name, directory, steps })
        })
        .collect();
    Some(Workflow { path: path.to_owned(), jobs })
}

/// Whether `on` names `push` or `pull_request` (or `pull_request_target`).
fn runs_on_changes(on: &Yaml) -> bool {
    let wanted = |event: &str| event == "push" || event.starts_with("pull_request");
    match on {
        Yaml::String(event) => wanted(event),
        Yaml::Array(events) => events.iter().filter_map(Yaml::as_str).any(wanted),
        Yaml::Hash(events) => events.keys().filter_map(Yaml::as_str).any(wanted),
        _ => false,
    }
}

/// A `run:` step that can run on a developer's computer, cleaned of setup.
fn step(step: &Yaml) -> Option<Step> {
    let run = step["run"].as_str()?;
    if let Some(shell) = step["shell"].as_str()
        && !matches!(shell.split_whitespace().next(), Some("bash" | "sh"))
    {
        return None;
    }
    let lines = shell_lines(run)?;
    let mut kept = Vec::new();
    for line in lines {
        if line.contains("${{") || line.contains("GITHUB_") || line.contains("RUNNER_") || is_control(&line) {
            return None;
        }
        if !is_setup(&line) {
            kept.push(line);
        }
    }
    if kept.is_empty() {
        return None;
    }
    let command = chain(kept);
    let name = step["name"].as_str().map_or_else(|| command.clone(), strip_expressions);
    let directory = step["working-directory"].as_str().map(str::to_owned);
    if directory.as_deref().is_some_and(|dir| dir.contains("${{")) {
        return None;
    }
    Some(Step { name, command, directory })
}

/// The commands of a `run:` script, one per entry, with `\` continuations
/// joined and comments and blank lines dropped; `None` for a heredoc.
fn shell_lines(run: &str) -> Option<Vec<String>> {
    let mut lines = Vec::new();
    let mut pending = String::new();
    for raw in run.lines() {
        let line = raw.trim();
        if line.contains("<<") {
            return None;
        }
        if let Some(head) = line.strip_suffix('\\') {
            pending.push_str(head.trim_end());
            pending.push(' ');
            continue;
        }
        pending.push_str(line);
        let full = std::mem::take(&mut pending);
        let full = full.trim();
        if !full.is_empty() && !full.starts_with('#') {
            lines.push(full.to_owned());
        }
    }
    let rest = pending.trim();
    if !rest.is_empty() {
        lines.push(rest.to_owned());
    }
    Some(lines)
}

/// Whether `line` is part of shell control flow, which does not survive
/// being chained with `&&`.
fn is_control(line: &str) -> bool {
    let first = line.split_whitespace().next().unwrap_or("");
    matches!(
        first,
        "if" | "then"
            | "else"
            | "elif"
            | "fi"
            | "for"
            | "while"
            | "until"
            | "do"
            | "done"
            | "case"
            | "esac"
            | "function"
            | "{"
            | "}"
    ) || line.ends_with('{')
        || line.ends_with("; then")
        || line.ends_with("; do")
}

/// Whether `line` sets up the CI machine rather than checks the code:
/// installs, toolchains, `echo`s and the like. A developer's computer is
/// set up already.
fn is_setup(line: &str) -> bool {
    let words: Vec<&str> = line.split_whitespace().collect();
    let first = words.first().copied().unwrap_or("");
    let second = words.get(1).copied().unwrap_or("");
    if matches!(
        first,
        "sudo"
            | "apt"
            | "apt-get"
            | "brew"
            | "choco"
            | "winget"
            | "yum"
            | "dnf"
            | "apk"
            | "rustup"
            | "echo"
            | "printf"
            | "export"
            | "curl"
            | "wget"
            | "git"
            | "ls"
            | "pwd"
            | "env"
            | "which"
            | "cat"
            | "corepack"
            | "nvm"
            | "set"
    ) {
        return true;
    }
    let installs = matches!(second, "install" | "i" | "ci" | "add" | "sync" | "download" | "fetch" | "config");
    match first {
        "npm" | "pnpm" | "bun" | "yarn" | "pip" | "pip3" | "pipx" | "gem" | "bundle" | "poetry" | "uv" | "composer"
        | "cargo" | "go" | "deno" => {
            installs
                || (first == "yarn" && second.is_empty())
                || (first == "go" && words.get(1..3) == Some(&["mod", "download"][..]))
                || (first == "uv" && second == "pip")
        }
        "python" | "python3" => words.get(1..4) == Some(&["-m", "pip", "install"][..]),
        "npx" | "bunx" => words.get(1..3).is_some_and(|w| w[0].contains("playwright") && w[1] == "install"),
        _ => false,
    }
}

/// `text` without `${{ … }}` expressions and the brackets left around
/// them: `test (${{ matrix.os }})` is `test`.
fn strip_expressions(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("${{") {
        out.push_str(&rest[..start]);
        rest = rest[start..].find("}}").map_or("", |end| &rest[start + end + 2..]);
    }
    out.push_str(rest);
    out.replace("()", "").replace("[]", "").split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for (path, text) in files {
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            fs::write(path, text).expect("write");
        }
        dir
    }

    fn commands(dir: &Path, checks: &[&str]) -> Vec<(String, String)> {
        detect(dir, checks).into_iter().map(|(check, found)| (check, found.command)).collect()
    }

    fn pair(check: &str, command: &str) -> (String, String) {
        (check.to_owned(), command.to_owned())
    }

    const RUST_CI: &str = r#"
name: CI
on:
  push:
    branches: [main]
  pull_request:
env:
  CARGO_TERM_COLOR: always
jobs:
  lint:
    name: Lint
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: rustup component add clippy rustfmt
      - name: Format
        run: cargo fmt --all --check
      - name: Clippy
        run: cargo clippy --workspace --all-targets -- -D warnings
  test:
    name: test (${{ matrix.os }})
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: Swatinem/rust-cache@v2
      - name: Install system libraries
        if: runner.os == 'Linux'
        run: sudo apt-get update && sudo apt-get install -y libxkbcommon-dev
      - name: Run tests
        run: cargo test --workspace --locked
"#;

    #[test]
    fn missing_lists_the_ci_steps_a_command_does_not_run() {
        assert_eq!(
            missing("cargo fmt --check", "cargo fmt --check && cargo clippy && cargo test"),
            ["cargo clippy", "cargo test"]
        );
        assert!(missing("cargo test && cargo fmt --check", "cargo fmt --check && cargo test").is_empty());
        assert!(missing("make ci", "make ci").is_empty());
        assert_eq!(missing("", "make ci"), ["make ci"]);
    }

    #[test]
    fn checks_get_the_job_that_matches_their_name() {
        let dir = repo(&[(".github/workflows/rust.yml", RUST_CI)]);
        assert_eq!(
            commands(dir.path(), &["tests", "lint", "format", "security"]),
            [
                pair("format", "cargo fmt --all --check"),
                pair("lint", "cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings"),
                pair("tests", "cargo test --workspace --locked"),
            ],
            "setup is dropped; a step matches when no job does; nothing is made up"
        );
        let found = detect(dir.path(), &["tests"]);
        assert_eq!(found["tests"].source, ".github/workflows/rust.yml");
    }

    #[test]
    fn the_whole_signoff_chains_every_checking_job() {
        let deploy = "on: push\njobs:\n  deploy:\n    steps:\n      - run: bin/deploy\n";
        let dir = repo(&[(".github/workflows/rust.yml", RUST_CI), (".github/workflows/deploy.yml", deploy)]);
        let found = detect(dir.path(), &["signoff"]);
        assert_eq!(
            found["signoff"].command,
            "cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test \
             --workspace --locked",
            "deploys are left out"
        );
        assert_eq!(found["signoff"].source, ".github/workflows/rust.yml");
    }

    #[test]
    fn rails_ci_steps_come_first_and_never_run_signoff_themselves() {
        let ci = r#"
# Run using bin/ci
CI.run do
  step "Setup", "bin/setup --skip-server"
  step "Style: Ruby", "bin/rubocop"
  step "Security: Gem audit", "bin/bundler-audit"
  step "Tests: Rails", "bin/rails", "test"
  step "Tests: Seeds", "env RAILS_ENV=test bin/rails db:seed:replant"
  step "Dynamic", "bin/#{name}"

  if success?
    step "Signoff: All systems go. Ready for merge and deploy.", "gh signoff"
  else
    failure "Signoff: CI failed. Do not merge or deploy.", "Fix the issues and try again."
  end
end
"#;
        let workflow = "on: [pull_request]\njobs:\n  test:\n    steps:\n      - run: bin/rails test:all\n";
        let dir = repo(&[("config/ci.rb", ci), (".github/workflows/ci.yml", workflow), ("bin/ci", "")]);
        assert_eq!(
            commands(dir.path(), &["lint", "security", "tests", "signoff"]),
            [
                pair("lint", "bin/rubocop"),
                pair("security", "bin/bundler-audit"),
                pair(
                    "signoff",
                    "bin/setup --skip-server && bin/rubocop && bin/bundler-audit && bin/rails test && env \
                     RAILS_ENV=test bin/rails db:seed:replant"
                ),
                pair("tests", "bin/rails test"),
            ]
        );
        assert_eq!(detect(dir.path(), &["tests"])["tests"].source, "config/ci.rb");
    }

    #[test]
    fn steps_that_need_github_are_skipped() {
        let workflow = r#"
on: pull_request
jobs:
  build:
    runs-on: ubuntu-latest
    defaults:
      run:
        working-directory: web
    steps:
      - run: npm ci
      - name: Lint
        run: npm run lint
      - name: Test
        run: |
          # unit tests
          npm test -- \
            --coverage
      - name: Report
        run: echo "done" >> $GITHUB_STEP_SUMMARY
      - name: Type check
        run: npx tsc --noEmit --project ${{ matrix.project }}
      - name: Loop
        run: |
          for f in a b; do
            echo $f
          done
      - name: Windows
        shell: pwsh
        run: Get-ChildItem
"#;
        let dir = repo(&[(".github/workflows/web.yml", workflow)]);
        assert_eq!(
            commands(dir.path(), &["lint", "test", "types", "build"]),
            [
                pair("build", "cd web && npm run lint && npm test -- --coverage"),
                pair("lint", "cd web && npm run lint"),
                pair("test", "cd web && npm test -- --coverage"),
            ]
        );
    }

    #[test]
    fn workflows_that_do_not_run_on_changes_are_ignored() {
        let nightly = "on:\n  schedule:\n    - cron: '0 0 * * *'\njobs:\n  test:\n    steps:\n      - run: make soak\n";
        let reusable = "on: pull_request\njobs:\n  test:\n    uses: org/repo/.github/workflows/t.yml@main\n";
        let broken = "on: [push\njobs: {";
        let dir = repo(&[
            (".github/workflows/nightly.yml", nightly),
            (".github/workflows/reusable.yaml", reusable),
            (".github/workflows/broken.yml", broken),
            (".github/workflows/README.md", "on: push"),
        ]);
        assert!(detect(dir.path(), &["test", "signoff"]).is_empty());
    }

    #[test]
    fn without_ci_close_matches_from_the_repository_files_count() {
        let dir = repo(&[("package.json", r#"{"scripts":{"test":"jest","lint":"eslint ."}}"#), ("yarn.lock", "")]);
        assert_eq!(
            commands(dir.path(), &["tests", "lint", "e2e", "signoff"]),
            [pair("lint", "yarn run lint"), pair("tests", "yarn run test")],
            "a check with no close match gets nothing"
        );
    }

    #[test]
    fn ruby_strings_read_literals_only() {
        assert_eq!(ruby_strings(r#""a", 'b c', "d\"e""#), Some(vec!["a".into(), "b c".into(), "d\"e".into()]));
        assert_eq!(ruby_strings(r#""a", "b") # note"#), Some(vec!["a".into(), "b".into()]));
        assert_eq!(ruby_strings("title, cmd"), None);
        assert_eq!(shell_quote("db:seed"), "db:seed");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn expressions_are_stripped_from_names() {
        assert_eq!(strip_expressions("test (${{ matrix.os }})"), "test");
        assert_eq!(strip_expressions("${{ matrix.name }} lint"), "lint");
        assert_eq!(strip_expressions("plain"), "plain");
    }
}
