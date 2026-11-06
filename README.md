# Signoff

Signoff is a tray companion for [`basecamp/gh-signoff`](https://github.com/basecamp/gh-signoff) on macOS, Windows and Linux. It keeps the open pull requests you care about one click away. It shows the branch-protection checks that actually gate each pull request and runs your local verification for each one. It writes the signoff only after that command succeeds.

It is a native Rust app built with [egui](https://github.com/emilk/egui) on [fastframe](https://github.com/crmne/fastframe), with no browser engine, Go or Node.

## What it does

- Tracks several local GitHub checkouts and shows either your pull requests or everyone's.
- The tray icon is a pair of eyes coloured by overall health. Click it to open the glance popover, which lists pull requests failing first, then waiting on you, then ready. Filter by Yours, All, Failing or Ready.
- Opens a pull request to show each required Signoff context and its latest status on the exact head commit.
- Maps every check to a local command and timeout. **Run & sign off** streams the output, then calls `gh signoff --commit <sha>` only on exit code 0.
- Refuses to run when the pull request's head commit isn't checked out locally, and checks again before signing off.
- Keeps a local history of runs, signoffs and rule changes beside the statuses GitHub reported.
- Edits only the Signoff contexts a branch requires. Reviews, restrictions and other required checks are left alone.
- Supports light and dark mode, reduced motion and launch at login.

## Requirements

- [GitHub CLI](https://cli.github.com/), signed in with `gh auth login`
- `gh extension install basecamp/gh-signoff`
- On Linux: `libxkbcommon-x11` for the window and a StatusNotifier host for the tray. GNOME needs the AppIndicator extension.

Signoff never reads or stores a GitHub token. Every GitHub read and write goes through `gh`, with your own sign-in.

## Install

Every push to `main` builds packages in the [Package workflow](../../actions/workflows/package.yml). Open the latest run and download the artifact for your OS:

| OS | Artifact | Contents |
| --- | --- | --- |
| macOS | `signoff-macos` | `Signoff.app` and a `.dmg` |
| Windows | `signoff-windows` | `Signoff_<version>_x64-setup.exe` |
| Linux | `signoff-linux` | an `.AppImage` and a `.deb` |

Pushing a `v*` tag also attaches them to a GitHub release. The packages aren't signed yet:

- **macOS:** right-click `Signoff.app` and choose Open the first time.
- **Windows:** in the SmartScreen prompt, choose More info, then Run anyway.

## Run from source

Rust 1.98 is pinned in `rust-toolchain.toml`, so rustup installs it for you.

```bash
cargo run -p signoff -- --demo      # sample data, no GitHub reads or writes
cargo run -p signoff                # your tracked repositories
cargo run -p signoff -- --hidden    # start in the tray without opening the window
```

The first run copies the config and history the previous Go/Electron version wrote, so tracked repositories carry over.

## Configure a repository

1. Open Settings from the sliders button on the popover's band, or from the tray menu.
2. Under **Repositories**, add a local checkout from the folder picker or paste its path. Choose **My pull requests** or **Everyone's**.
3. Under **Commands**, give each check the command that proves it, for example `cargo test --workspace` or `bin/lint`. Checks that your pull requests require are offered with one click.
4. Check out the pull request's branch, open the pull request in the popover and press **Run & sign off**.

Commands come only from your local config. Nothing from GitHub can supply or change them. **Settings → General** shows where the config file lives.

## Branch rules

**Settings → Branch rules** reads a branch's protection through `gh` and lists the Signoff checks it requires. You can add or remove checks there and save the change back to GitHub:

- Required checks that aren't Signoff checks are shown as locked and kept.
- Reviews, restrictions and the rest of the protection rule are untouched.
- Removing every Signoff check leaves the protection rule in place.
- A branch with no protection is set up with `gh signoff install`.
- Protection that can't be read is never written.

## Project layout

- `crates/core` (`signoff-core`): the logic, with no UI. It covers `gh` reads, branch-protection resolution and editing, config and history, the multi-repository state, verify and sign off, and the demo data.
- `crates/ui` (`signoff-ui`): the design system, with the palette, type, icons and widgets. Run `cargo run -p signoff-ui --example gallery` to see them all.
- `crates/app` (`signoff`): the tray app, popover and settings window.

## Development

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`cargo test` includes UI tests (`crates/app/src/ui_tests.rs`). They drive the real popover and settings through their accessibility tree with [egui_kittest](https://crates.io/crates/egui_kittest), with no window, GPU or GitHub. `gh` and `git` are answered by a script that records each call, so the tests check exactly what would be sent to GitHub.

To build packages locally, install [cargo-packager](https://github.com/crabnebula-dev/cargo-packager) and run:

```bash
cargo packager --release -p signoff --formats deb   # or app,dmg / nsis / appimage
```

They are written to `target/packages/`.

## License

MIT
