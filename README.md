# gh-signoff-ui

A companion window for [`basecamp/gh-signoff`](https://github.com/basecamp/gh-signoff).

`gh signoff` is one word to type. Knowing *whether you still need to type it* is
the part that sends you to the GitHub PR page. This shows that in a glance:
what's required to merge, what's signed off, and what's blocking.

```bash
gh signoff-ui
```

Opens a local window against the repo you're standing in. Try it without a repo:

```bash
gh signoff-ui -demo
```

## What it does

- Shows the current branch's PR, its required signoff contexts, and which are done
- Signs off a context and streams the real command's output
- Shows the branch protection rules that actually gate the merge
- Refreshes on its own while focused

## What it doesn't do

- **Reimplement signoff.** Reads come from the GitHub API. The one write path
  shells out to the real `gh signoff`, so its own preconditions (clean tree,
  commit pushed to a remote) still apply.
- **Run your tests.** `gh signoff` is an attestation that *you* ran them. A
  button that ran the suite and signed off in one motion would quietly turn
  that attestation into unattended CI, which is the thing gh-signoff exists to
  avoid. Sign off means sign off.
- **Handle your credentials.** Auth stays in `gh`'s keyring; this never sees a
  token.
- **Edit branch protection.** Protection is shown read-only. `gh signoff
  install` rewrites the whole required-checks list, so a per-check toggle would
  have to silently re-issue the entire command. It shows you the command to run
  instead.

## Install

```bash
gh extension install asrat77/gh-signoff-ui
```

Or build it:

```bash
go build -o gh-signoff-ui . && ./gh-signoff-ui
```

## Flags

| Flag | Meaning |
|---|---|
| `-dir` | repository directory (default `.`) |
| `-port` | port to bind (default: a free one) |
| `-demo` | serve sample data; never reads or writes GitHub |
| `-read-only` | refuse every write |
| `-no-browser` | don't open a browser |

## Notes

**Requirements are read from branch protection, not from `gh signoff status`.**
That command always lists the bare `signoff` context, so on a repo requiring
only `signoff/tests` it prints a permanent `✗ signoff`. Fine in a terminal,
wrong for a window whose job is answering "can I merge". When protection can't
be read — that endpoint needs admin — it falls back to the CLI's assumption and
says so rather than inventing a number.

**Loopback isn't a trust boundary.** Any page in your browser can reach
`127.0.0.1`, so every API route requires a per-run token and a loopback origin.

**History covers the current commit.** Commit statuses carry their author and
timestamp, so that page is real data — but scoped to the PR head, not the whole
repo. Anything broader needs a data source that doesn't exist yet.
