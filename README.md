# Signoff

Signoff is a macOS menu-bar companion for [`basecamp/gh-signoff`](https://github.com/basecamp/gh-signoff). It keeps the open pull requests you care about one click away, shows the branch-protection checks that actually gate each PR, runs your configured local verification, and writes the signoff only after that command succeeds.

This is the product, not a browser dashboard. The normal experience is a compact menu-bar popover with a separate native settings window.

## Product capabilities

- Track multiple local GitHub repositories and either your PRs or every open PR.
- Sort the glance view by failing, needs-attention, and ready states.
- Inspect required Signoff contexts and the latest status attached to the exact PR head commit.
- Map every context to an explicit local command and timeout.
- Stream command output, then call the real `gh signoff --commit <sha>` only after exit code 0.
- Refuse to run when the selected PR commit is not the commit checked out locally, and verify it again before signing off.
- Keep local run/signoff history alongside GitHub commit-status history.
- Edit only Signoff status-check contexts while preserving reviews, restrictions, and unrelated required checks.
- Reflect overall health in the colored menu-bar icon.
- Work in light mode, dark mode, and reduced-motion environments.

## Requirements

- macOS on Apple silicon for the current local package target
- Node.js 22.12 or newer
- Go 1.24 or newer
- [GitHub CLI](https://cli.github.com/) authenticated with `gh auth login`
- `gh extension install basecamp/gh-signoff`

Signoff never reads or stores a GitHub token. Authentication remains owned by `gh` and the operating-system keychain.

## Run from source

```bash
npm install
npm start
```

The app appears in the menu bar and does not add a Dock icon. On first launch, open Settings and choose one or more local Git checkouts.

For the complete multi-repository sample without GitHub reads or writes:

```bash
npm run start:demo
```

## Configure a repository

1. Add its local checkout under **Settings → Repositories**.
2. Choose whether to watch **My PRs** or **All open PRs**.
3. Under **Commands**, map each discovered Signoff context to the command that proves it, for example `bin/test`, `bin/lint`, or `bin/security`.
4. Check out the PR branch locally. A command is enabled only when `HEAD` equals that PR's head SHA.
5. Use **Run & sign off** from the PR focus view.

Command text is accepted only through local settings; a run request cannot supply or override it. Configuration and history are stored with private file permissions in:

```text
~/Library/Application Support/Signoff/config.json
~/Library/Application Support/Signoff/history.json
```

## Branch rules

The Branch rules page changes only required status checks:

- existing non-Signoff contexts are retained;
- reviews, restrictions, and the rest of the protection rule are untouched;
- removing every Signoff context does not delete branch protection;
- an unreadable protection rule is displayed but cannot be edited.

The real write is made through GitHub's branch-protection status-check subresources. Signoff uses `gh signoff install` only when the branch has no protection rule yet.

## Build the macOS app

```bash
npm run pack:mac
open dist/mac-arm64/Signoff.app
```

The local package is written to `dist/mac-arm64/Signoff.app` with the branded icon, the Electron host, and the Go service embedded. It is intentionally unsigned in this repository; public distribution still requires a Developer ID certificate, hardened-runtime signing, and notarization.

## Development and verification

```bash
npm test
go test -race ./...
npm run test:ui
```

`npm test` runs the Go test suite, `go vet`, and JavaScript syntax checks for the renderer, main process, and preload bridge.

`npm run test:ui` starts the embedded demo service and runs Playwright checks for the glance view, multi-repository data, run-and-sign-off flow, checked-out-commit guard, fuzzy palette, all settings pages, dark mode, and reduced motion. On macOS it uses the installed Google Chrome; on CI install Playwright's browser with `npx playwright install chromium`.

The Go service remains directly runnable for diagnostics:

```bash
go run . -demo
```

Useful service flags include `-dir`, `-port`, `-demo`, `-read-only`, `-no-browser`, and `-config`. `-desktop` and `-startup-json` are internal desktop-host integration flags.

## Security model

- The service binds only to `127.0.0.1` on a random port.
- Every API route requires a random per-process header token and rejects non-loopback browser origins.
- The token is injected into the local page, never placed in a URL or written to desktop logs.
- Electron renderers use context isolation, sandboxing, no Node integration, a narrow preload API, and a local-only content-security policy.
- External navigation is denied except for explicit `https://github.com` links opened in the system browser.
- A local command and the PR commit are both revalidated before the GitHub signoff write.

Readiness comes from the branch-protection contexts for the PR's base branch. When protection cannot be read, Signoff falls back to the extension's bare `signoff` assumption and labels that uncertainty instead of reporting a false green.
