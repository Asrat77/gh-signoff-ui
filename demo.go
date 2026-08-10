package main

import (
	"sync"
	"time"
)

// Demo mode serves a fixed in-memory state instead of reading GitHub.
//
// It exists so the interaction loop can be exercised without a repo that has
// signoff protection configured, and so anyone evaluating the design can see
// the dashboard without setting one up. It is always clearly labelled in the
// UI, never writes anything, and is not a fallback: real mode never degrades
// into demo mode.
type demoStore struct {
	mu    sync.Mutex
	state State
}

func newDemoStore() *demoStore {
	now := time.Now()
	ago := func(d time.Duration) *time.Time { t := now.Add(-d); return &t }

	return &demoStore{state: State{
		Env: Environment{
			GhInstalled: true, GhVersion: "2.95.0", Authenticated: true,
			Login: "you", InGitRepo: true,
			SignoffInstalled: true, SignoffVersion: "0.3.0",
		},
		Repo: RepoInfo{
			NameWithOwner: "basecamp/gh-signoff",
			DefaultBranch: "main",
			CurrentBranch: "feature/partial-checks",
			HeadSHA:       "3db401c2f8692ed736d2b5f5981a9ff99091d734",
		},
		PR: &PRInfo{
			Number: 482, Title: "Add label-based partial signoff",
			Author: "you", URL: "https://github.com/basecamp/gh-signoff/pull/482",
			State: "OPEN", BaseRef: "main", Mergeable: "MERGEABLE",
		},
		Rules: Rules{
			Branch:   "main",
			Contexts: []string{"signoff/tests", "signoff/lint", "signoff/security"},
			SignoffContexts: []string{
				"signoff/tests", "signoff/lint", "signoff/security",
			},
			Readable: true, Protected: true, Enforced: true,
			InstallCommand: "gh signoff install tests lint security --branch main",
		},
		Checks: []Check{
			{Context: "signoff/tests", Name: "tests", Category: "tests",
				State: "success", Required: true, Actor: "you",
				At: ago(4 * time.Minute), Command: "gh signoff tests"},
			{Context: "signoff/lint", Name: "lint", Category: "lint",
				State: "success", Required: true, Actor: "you",
				At: ago(4 * time.Minute), Command: "gh signoff lint"},
			{Context: "signoff/security", Name: "security", Category: "security",
				State: "absent", Required: true, Command: "gh signoff security"},
		},
		History: []Event{
			{Name: "tests", State: "success", Actor: "you", At: now.Add(-4 * time.Minute)},
			{Name: "lint", State: "success", Actor: "you", At: now.Add(-4 * time.Minute)},
			{Name: "security", State: "failure", Actor: "you",
				At: now.Add(-22 * time.Minute), Detail: "3 vulnerabilities found"},
		},
		Passed: 2, Total: 3, Blocking: "security",
	}}
}

func (d *demoStore) snapshot() State {
	d.mu.Lock()
	defer d.mu.Unlock()

	st := d.state
	st.FetchedAt = time.Now()
	st.Warning = "Demo data — nothing here is read from or written to GitHub."

	checks := make([]Check, len(d.state.Checks))
	copy(checks, d.state.Checks)
	st.Checks = checks

	st.Passed, st.Total, st.Blocking = 0, 0, ""
	for _, c := range checks {
		if !c.Required {
			continue
		}
		st.Total++
		if c.State == "success" {
			st.Passed++
		} else if st.Blocking == "" {
			st.Blocking = c.Name
		}
	}
	st.Ready = st.Total > 0 && st.Passed == st.Total
	st.normalize()
	return st
}

// signoff marks a context successful, so the full click-to-merge-CTA loop can
// be exercised end to end.
func (d *demoStore) signoff(name string) {
	d.mu.Lock()
	defer d.mu.Unlock()

	if name == "" {
		name = "signoff"
	}
	now := time.Now()
	for i := range d.state.Checks {
		if d.state.Checks[i].Name == name {
			d.state.Checks[i].State = "success"
			d.state.Checks[i].Actor = "you"
			d.state.Checks[i].At = &now
			d.state.History = append([]Event{{
				Name: name, State: "success", Actor: "you", At: now,
			}}, d.state.History...)
		}
	}
}
