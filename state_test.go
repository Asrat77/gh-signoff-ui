package main

import (
	"encoding/json"
	"testing"
	"time"
)

func status(state string) CommitStatus {
	return CommitStatus{State: state, Creator: "you", CreatedAt: time.Now()}
}

func TestResolveChecksUsesProtectionWhenReadable(t *testing.T) {
	checks := resolveChecks(
		[]string{"signoff/tests", "signoff/lint"},
		map[string]CommitStatus{"signoff/tests": status("success")},
		true,
	)

	if len(checks) != 2 {
		t.Fatalf("got %d checks, want 2: %+v", len(checks), checks)
	}
	for _, c := range checks {
		if !c.Required {
			t.Errorf("%s should be required", c.Name)
		}
	}

	passed, total, blocking, ready := tally(checks)
	if passed != 1 || total != 2 || blocking != "lint" || ready {
		t.Errorf("tally = (%d, %d, %q, %v), want (1, 2, \"lint\", false)",
			passed, total, blocking, ready)
	}
}

// The bare "signoff" context must NOT be treated as required when protection
// is readable and does not list it. gh signoff status always shows it, which
// would render a permanent failure on repos that only require signoff/<name>.
func TestBareSignoffNotRequiredWhenProtectionOmitsIt(t *testing.T) {
	checks := resolveChecks([]string{"signoff/tests"},
		map[string]CommitStatus{"signoff/tests": status("success")}, true)

	_, total, _, ready := tally(checks)
	if total != 1 {
		t.Errorf("total = %d, want 1 (only signoff/tests gates merge)", total)
	}
	if !ready {
		t.Error("should be ready: the only required context passed")
	}
}

// When protection cannot be read we fall back to assuming the default context
// is required, erring toward "not ready" rather than a false green.
func TestUnreadableProtectionFallsBackToBareSignoff(t *testing.T) {
	checks := resolveChecks(nil, map[string]CommitStatus{}, false)

	if len(checks) != 1 || checks[0].Context != "signoff" {
		t.Fatalf("got %+v, want a single bare signoff check", checks)
	}
	if !checks[0].Required {
		t.Error("fallback check must be required")
	}
	if _, _, _, ready := tally(checks); ready {
		t.Error("must not report ready when the assumed context is unsigned")
	}
}

// A green optional context must never make a blocked PR look mergeable.
func TestOptionalContextShownButNotCounted(t *testing.T) {
	checks := resolveChecks(
		[]string{"signoff/tests"},
		map[string]CommitStatus{
			"signoff/tests": status("absent"),
			"signoff/docs":  status("success"), // signed off, not required
		},
		true,
	)

	if len(checks) != 2 {
		t.Fatalf("got %d checks, want 2 (required + optional)", len(checks))
	}

	var docs *Check
	for i := range checks {
		if checks[i].Name == "docs" {
			docs = &checks[i]
		}
	}
	if docs == nil {
		t.Fatal("optional signed-off context should still be listed")
	}
	if docs.Required {
		t.Error("docs is not in protection and must not be required")
	}

	passed, total, _, ready := tally(checks)
	if passed != 0 || total != 1 || ready {
		t.Errorf("tally = (%d, %d, ready=%v), want (0, 1, false)", passed, total, ready)
	}
}

// Only "success" counts. A failing or pending status is not a signoff.
func TestNonSuccessStatesDoNotCount(t *testing.T) {
	for _, state := range []string{"failure", "error", "pending", "absent"} {
		checks := resolveChecks([]string{"signoff/tests"},
			map[string]CommitStatus{"signoff/tests": status(state)}, true)
		if _, _, _, ready := tally(checks); ready {
			t.Errorf("state %q must not count as signed off", state)
		}
	}
}

func TestDisplayNameAndCommand(t *testing.T) {
	cases := []struct{ ctx, name, cmd string }{
		{"signoff", "signoff", "gh signoff"},
		{"signoff/tests", "tests", "gh signoff tests"},
		{"signoff/design-review", "design-review", "gh signoff design-review"},
	}
	for _, c := range cases {
		if got := displayName(c.ctx); got != c.name {
			t.Errorf("displayName(%q) = %q, want %q", c.ctx, got, c.name)
		}
		if got := signoffCommand(c.ctx); got != c.cmd {
			t.Errorf("signoffCommand(%q) = %q, want %q", c.ctx, got, c.cmd)
		}
	}
}

func TestBuildInstallCommand(t *testing.T) {
	got := buildInstallCommand([]string{"signoff/tests", "signoff/lint"}, "main")
	want := "gh signoff install tests lint --branch main"
	if got != want {
		t.Errorf("got %q, want %q", got, want)
	}

	// The bare context is implied by `gh signoff install` and must not appear
	// as a positional argument.
	got = buildInstallCommand([]string{"signoff"}, "main")
	want = "gh signoff install --branch main"
	if got != want {
		t.Errorf("got %q, want %q", got, want)
	}
}

// Regression: nil slices marshalled as null and blew up the front end on
// .length. Every slice must reach the browser as [].
func TestNormalizeEmitsEmptyArrays(t *testing.T) {
	var st State
	st.normalize()

	b, err := json.Marshal(st)
	if err != nil {
		t.Fatal(err)
	}
	var round map[string]json.RawMessage
	if err := json.Unmarshal(b, &round); err != nil {
		t.Fatal(err)
	}

	for _, key := range []string{"checks", "history"} {
		if string(round[key]) != "[]" {
			t.Errorf("%s = %s, want []", key, round[key])
		}
	}
	var rules map[string]json.RawMessage
	if err := json.Unmarshal(round["rules"], &rules); err != nil {
		t.Fatal(err)
	}
	for _, key := range []string{"contexts", "signoffContexts"} {
		if string(rules[key]) != "[]" {
			t.Errorf("rules.%s = %s, want []", key, rules[key])
		}
	}
}

// Context names reach the shell as arguments to gh, so anything that could
// change the command's meaning must be rejected.
func TestValidContextNameRejectsShellMetacharacters(t *testing.T) {
	bad := []string{
		"tests;rm -rf /", "tests rm", "--commit", "$(whoami)", "`id`",
		"tests|cat", "tests&", "a/b", "tests\n", "'quoted'",
	}
	for _, s := range bad {
		if validContextName(s) {
			t.Errorf("validContextName(%q) = true, want false", s)
		}
	}
	for _, s := range []string{"tests", "design-review", "lint_2", "v1.2"} {
		if !validContextName(s) {
			t.Errorf("validContextName(%q) = false, want true", s)
		}
	}
}

func TestDemoSignoffCompletesTheRing(t *testing.T) {
	d := newDemoStore()

	if st := d.snapshot(); st.Ready {
		t.Fatal("demo should start not ready")
	}
	d.signoff("security")

	st := d.snapshot()
	if !st.Ready || st.Passed != st.Total {
		t.Errorf("after signing off security: passed=%d total=%d ready=%v, want all passed",
			st.Passed, st.Total, st.Ready)
	}
}

// GitHub returned creator: null for statuses written by gh signoff through a
// user OAuth token, which rendered the author as "unknown". gh-signoff puts
// the name in the description instead.
func TestActorFallsBackToDescription(t *testing.T) {
	cases := []struct {
		creator, desc, want string
	}{
		{"octocat", "Asrat signed off", "octocat"}, // creator wins
		{"", "Asrat signed off", "Asrat"},          // observed on real data
		{"", "", ""},
		{"", "something else entirely", ""},
	}
	for _, c := range cases {
		got := actorFrom(CommitStatus{Creator: c.creator, Description: c.desc})
		if got != c.want {
			t.Errorf("actorFrom(creator=%q, desc=%q) = %q, want %q",
				c.creator, c.desc, got, c.want)
		}
	}
}
