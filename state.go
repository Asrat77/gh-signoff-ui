package main

import (
	"context"
	"sort"
	"strings"
	"time"
)

// Check is one signoff context resolved against the current commit.
type Check struct {
	Context     string     `json:"context"` // "signoff" or "signoff/tests"
	Name        string     `json:"name"`    // display name: "signoff" or "tests"
	Category    string     `json:"category"`
	State       string     `json:"state"` // success | failure | error | pending | absent
	Required    bool       `json:"required"`
	Actor       string     `json:"actor"`
	AvatarURL   string     `json:"avatarUrl"`
	At          *time.Time `json:"at"`
	Description string     `json:"description"`
	Command     string     `json:"command"` // the exact CLI command that signs this off
}

// State is the whole payload the dashboard renders from.
type State struct {
	Env       Environment `json:"env"`
	Repo      RepoInfo    `json:"repo"`
	PR        *PRInfo     `json:"pr"`
	Checks    []Check     `json:"checks"`
	History   []Event     `json:"history"`
	Rules     Rules       `json:"rules"`
	Passed    int         `json:"passed"`
	Total     int         `json:"total"`
	Ready     bool        `json:"ready"`
	Blocking  string      `json:"blocking"` // display name of the first unmet required check
	Warning   string      `json:"warning"`  // non-fatal caveat shown under the header
	Error     string      `json:"error"`
	FetchedAt time.Time   `json:"fetchedAt"`
}

// Rules describes what branch protection actually requires.
type Rules struct {
	Branch          string   `json:"branch"`
	Contexts        []string `json:"contexts"`        // every required context, signoff or not
	SignoffContexts []string `json:"signoffContexts"` // just the signoff/* ones
	Readable        bool     `json:"readable"`        // false only when the API refused (needs admin)
	Protected       bool     `json:"protected"`       // true when the branch has protection rules at all
	Enforced        bool     `json:"enforced"`        // true when at least one signoff context gates merge
	InstallCommand  string   `json:"installCommand"`
}

// Event is one entry of real signoff activity.
type Event struct {
	Name   string    `json:"name"`
	State  string    `json:"state"`
	Actor  string    `json:"actor"`
	At     time.Time `json:"at"`
	Detail string    `json:"detail"`
}

// displayName mirrors gh-signoff's own rendering: "signoff/tests" shows as
// "tests", and the bare default context keeps the name "signoff".
func displayName(ctx string) string {
	if ctx == "signoff" {
		return "signoff"
	}
	return strings.TrimPrefix(ctx, "signoff/")
}

// categoryFor drives badge color only. v1 ships three known categories and
// falls back to a stable slot for anything a team invents (spec §7).
func categoryFor(name string) string {
	switch name {
	case "tests", "test":
		return "tests"
	case "lint", "rubocop", "standard", "format":
		return "lint"
	case "security", "audit", "brakeman":
		return "security"
	case "signoff":
		return "default"
	}
	// Stable per-name fallback so a custom category keeps one color.
	var sum int
	for _, r := range name {
		sum += int(r)
	}
	return []string{"alt1", "alt2", "alt3"}[sum%3]
}

// signoffCommand returns the exact command a user would type to sign this off,
// which the UI shows verbatim. Teaching the CLI is a stated goal (spec §8.3).
func signoffCommand(ctx string) string {
	if ctx == "signoff" {
		return "gh signoff"
	}
	return "gh signoff " + strings.TrimPrefix(ctx, "signoff/")
}

// buildState assembles the dashboard payload.
//
// Which checks are *required* is the one place this program deliberately
// departs from `gh signoff status`. That command always seeds its list with the
// bare "signoff" context, so on a repo whose protection requires only
// "signoff/tests" it prints a permanent ✗ for "signoff". That is fine for a
// human reading a terminal, but this UI answers "can I merge", and merge is
// gated by branch protection alone. So:
//
//   - protection readable -> required = the signoff contexts protection lists
//   - protection unreadable (no admin) -> fall back to the CLI's assumption and
//     flag the readiness as unverified rather than inventing a number
//
// Contexts present on the commit but not required are still shown, marked
// not-required, because seeing a stray signoff is useful.
func buildState(ctx context.Context, dir string) State {
	st := State{FetchedAt: time.Now()}
	st.Env = probeEnvironment(ctx, dir)

	if !st.Env.GhInstalled || !st.Env.Authenticated || !st.Env.InGitRepo {
		return st // the UI renders a setup screen from Env alone
	}

	repo, err := readRepo(ctx, dir)
	if err != nil {
		st.Error = "Could not read this repository: " + err.Error()
		return st
	}
	st.Repo = repo

	pr, err := readPR(ctx, dir)
	if err != nil {
		st.Warning = "Could not look up a pull request: " + err.Error()
	}
	st.PR = pr

	// Statuses live on the PR head when there is a PR, otherwise on local HEAD.
	sha := repo.HeadSHA
	if pr != nil && pr.HeadSHA != "" {
		sha = pr.HeadSHA
	}
	if sha == "" {
		st.Error = "No commit to check — this branch has no HEAD yet."
		return st
	}

	// gh-signoff reads protection from the *default* branch unless told
	// otherwise; when a PR exists its base branch is the real merge gate.
	protBranch := repo.DefaultBranch
	if pr != nil && pr.BaseRef != "" {
		protBranch = pr.BaseRef
	}

	contexts, protStatus := readProtectionContexts(ctx, dir, repo.NameWithOwner, protBranch)
	readable := protStatus != ProtectionUnreadable
	rules := Rules{
		Branch:    protBranch,
		Contexts:  contexts,
		Readable:  readable,
		Protected: protStatus == ProtectionOK,
	}
	for _, c := range contexts {
		if strings.HasPrefix(c, "signoff") {
			rules.SignoffContexts = append(rules.SignoffContexts, c)
		}
	}
	rules.Enforced = len(rules.SignoffContexts) > 0
	rules.InstallCommand = buildInstallCommand(rules.SignoffContexts, protBranch)
	st.Rules = rules

	statuses, err := readCommitStatuses(ctx, dir, repo.NameWithOwner, sha)
	if err != nil {
		st.Error = "Could not read commit status: " + err.Error()
		return st
	}

	// Latest status wins per context; the API returns newest first but we do
	// not rely on that.
	latest := map[string]CommitStatus{}
	for _, s := range statuses {
		if !strings.HasPrefix(s.Context, "signoff") {
			continue
		}
		if prev, ok := latest[s.Context]; !ok || s.CreatedAt.After(prev.CreatedAt) {
			latest[s.Context] = s
		}
	}

	if !readable {
		st.Warning = "Branch protection isn't readable with your permissions, so required checks are assumed. Readiness may not match what GitHub enforces."
	}

	st.Checks = resolveChecks(rules.SignoffContexts, latest, readable)
	st.Passed, st.Total, st.Blocking, st.Ready = tally(st.Checks)

	// History is real: /statuses returns every status ever posted to this SHA,
	// including re-signoffs and failures, newest first.
	for _, s := range statuses {
		if !strings.HasPrefix(s.Context, "signoff") {
			continue
		}
		st.History = append(st.History, Event{
			Name:   displayName(s.Context),
			State:  s.State,
			Actor:  s.Creator,
			At:     s.CreatedAt,
			Detail: s.Description,
		})
	}
	sort.SliceStable(st.History, func(i, j int) bool {
		return st.History[i].At.After(st.History[j].At)
	})

	return st
}

// resolveChecks turns branch protection plus the statuses actually present on
// a commit into the rows the dashboard shows.
//
// Required membership comes from protection when it is readable. When it is
// not, it falls back to gh-signoff's own assumption that the bare "signoff"
// context is required, which is the safest guess: it reports "not ready" for
// a commit that might in fact be mergeable, rather than the reverse.
//
// Contexts signed off but not required are included and marked not-required,
// so a stray or newly removed context stays visible instead of disappearing.
func resolveChecks(signoffContexts []string, latest map[string]CommitStatus, readable bool) []Check {
	required := map[string]bool{}
	if readable {
		for _, c := range signoffContexts {
			required[c] = true
		}
	} else {
		required["signoff"] = true
	}

	seen := map[string]bool{}
	checks := []Check{}

	add := func(cname string) {
		if seen[cname] {
			return
		}
		seen[cname] = true

		name := displayName(cname)
		c := Check{
			Context:  cname,
			Name:     name,
			Category: categoryFor(name),
			State:    "absent",
			Required: required[cname],
			Command:  signoffCommand(cname),
		}
		if s, ok := latest[cname]; ok {
			c.State = s.State
			c.Actor = s.Creator
			c.AvatarURL = s.AvatarURL
			c.Description = s.Description
			at := s.CreatedAt
			c.At = &at
		}
		checks = append(checks, c)
	}

	if readable {
		for _, c := range signoffContexts {
			add(c)
		}
	} else {
		add("signoff")
	}

	// Map iteration order is random, so collect and sort before adding.
	extra := make([]string, 0, len(latest))
	for c := range latest {
		if !seen[c] {
			extra = append(extra, c)
		}
	}
	sort.Strings(extra)
	for _, c := range extra {
		add(c)
	}

	sort.SliceStable(checks, func(i, j int) bool {
		if checks[i].Required != checks[j].Required {
			return checks[i].Required
		}
		return checks[i].Name < checks[j].Name
	})
	return checks
}

// tally counts only required checks: an optional context that happens to be
// green must never make a blocked PR look ready.
func tally(checks []Check) (passed, total int, blocking string, ready bool) {
	for _, c := range checks {
		if !c.Required {
			continue
		}
		total++
		if c.State == "success" {
			passed++
		} else if blocking == "" {
			blocking = c.Name
		}
	}
	return passed, total, blocking, total > 0 && passed == total
}

// normalize guarantees every slice marshals as [] rather than null.
//
// Go encodes a nil slice as JSON null, which forces every consumer to guard
// each field before touching .length. Emitting empty arrays keeps the wire
// contract free of that null-versus-empty ambiguity.
func (s *State) normalize() {
	if s.Checks == nil {
		s.Checks = []Check{}
	}
	if s.History == nil {
		s.History = []Event{}
	}
	if s.Rules.Contexts == nil {
		s.Rules.Contexts = []string{}
	}
	if s.Rules.SignoffContexts == nil {
		s.Rules.SignoffContexts = []string{}
	}
}

// buildInstallCommand reconstructs the `gh signoff install` invocation that
// would produce the current rule, so the Required Checks page can show it.
func buildInstallCommand(signoffContexts []string, branch string) string {
	var names []string
	for _, c := range signoffContexts {
		if c == "signoff" {
			continue
		}
		names = append(names, strings.TrimPrefix(c, "signoff/"))
	}
	cmd := "gh signoff install"
	if len(names) > 0 {
		cmd += " " + strings.Join(names, " ")
	}
	if branch != "" {
		cmd += " --branch " + branch
	}
	return cmd
}
