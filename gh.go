package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os/exec"
	"strings"
	"time"
)

// run executes a command and returns stdout, stderr and the exit code.
// Every shell-out in this program goes through here so that auth, PATH and
// timeouts are handled in exactly one place.
func run(ctx context.Context, name string, args ...string) (string, string, int, error) {
	cmd := exec.CommandContext(ctx, name, args...)
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr

	err := cmd.Run()
	code := 0
	if err != nil {
		var ee *exec.ExitError
		if errors.As(err, &ee) {
			code = ee.ExitCode()
			err = nil // a non-zero exit is data, not a failure to launch
		}
	}
	return strings.TrimSpace(stdout.String()), strings.TrimSpace(stderr.String()), code, err
}

// ghJSON runs a gh subcommand and unmarshals its stdout into v.
func ghJSON(ctx context.Context, v any, args ...string) error {
	stdout, stderr, code, err := run(ctx, "gh", args...)
	if err != nil {
		return fmt.Errorf("launching gh: %w", err)
	}
	if code != 0 {
		msg := stderr
		if msg == "" {
			msg = stdout
		}
		return &ghError{Args: args, Code: code, Msg: msg}
	}
	if v == nil {
		return nil
	}
	return json.Unmarshal([]byte(stdout), v)
}

type ghError struct {
	Args []string
	Code int
	Msg  string
}

func (e *ghError) Error() string {
	return fmt.Sprintf("gh %s: exit %d: %s", strings.Join(e.Args, " "), e.Code, e.Msg)
}

// notFound reports whether the error is a 404/403 from the GitHub API.
// Branch protection returns one of these for repos where the caller lacks
// admin, which is the common case for contributors and must not be fatal.
func notFound(err error) bool {
	var ge *ghError
	if !errors.As(err, &ge) {
		return false
	}
	m := ge.Msg
	return strings.Contains(m, "404") || strings.Contains(m, "Not Found") ||
		strings.Contains(m, "403") || strings.Contains(m, "Branch not protected") ||
		strings.Contains(m, "Resource not accessible")
}

// ---------------------------------------------------------------------------
// Environment probing
// ---------------------------------------------------------------------------

// Environment captures everything that can be wrong before we can show a
// dashboard at all. The UI renders a specific remedy for each false flag
// rather than a generic error screen.
type Environment struct {
	GhInstalled      bool   `json:"ghInstalled"`
	GhVersion        string `json:"ghVersion"`
	Authenticated    bool   `json:"authenticated"`
	Login            string `json:"login"`
	InGitRepo        bool   `json:"inGitRepo"`
	SignoffInstalled bool   `json:"signoffInstalled"`
	SignoffVersion   string `json:"signoffVersion"`
}

func probeEnvironment(ctx context.Context, dir string) Environment {
	var env Environment

	if out, _, code, err := run(ctx, "gh", "--version"); err == nil && code == 0 {
		env.GhInstalled = true
		if line, _, ok := strings.Cut(out, "\n"); ok {
			env.GhVersion = strings.TrimPrefix(line, "gh version ")
		} else {
			env.GhVersion = strings.TrimPrefix(out, "gh version ")
		}
	}

	var acct struct {
		Login string `json:"login"`
	}
	if err := ghJSON(ctx, &acct, "api", "user", "--jq", "{login: .login}"); err == nil {
		env.Authenticated = acct.Login != ""
		env.Login = acct.Login
	}

	if _, _, code, err := runIn(ctx, dir, "git", "rev-parse", "--git-dir"); err == nil && code == 0 {
		env.InGitRepo = true
	}

	// `gh signoff version` prints "gh-signoff 0.3.0". A non-zero exit means the
	// extension is not installed.
	if out, _, code, err := runIn(ctx, dir, "gh", "signoff", "version"); err == nil && code == 0 {
		env.SignoffInstalled = true
		env.SignoffVersion = strings.TrimSpace(strings.TrimPrefix(out, "gh-signoff"))
	}

	return env
}

// runIn is run() with a working directory, needed because every git and gh
// call is relative to the repo the user launched us from.
func runIn(ctx context.Context, dir, name string, args ...string) (string, string, int, error) {
	return runInWithInput(ctx, dir, "", name, args...)
}

func runInWithInput(ctx context.Context, dir, input, name string, args ...string) (string, string, int, error) {
	cmd := exec.CommandContext(ctx, name, args...)
	cmd.Dir = dir
	if input != "" {
		cmd.Stdin = strings.NewReader(input)
	}
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr

	err := cmd.Run()
	code := 0
	if err != nil {
		var ee *exec.ExitError
		if errors.As(err, &ee) {
			code = ee.ExitCode()
			err = nil
		}
	}
	return strings.TrimSpace(stdout.String()), strings.TrimSpace(stderr.String()), code, err
}

func ghJSONIn(ctx context.Context, dir string, v any, args ...string) error {
	stdout, stderr, code, err := runIn(ctx, dir, "gh", args...)
	if err != nil {
		return fmt.Errorf("launching gh: %w", err)
	}
	if code != 0 {
		msg := stderr
		if msg == "" {
			msg = stdout
		}
		return &ghError{Args: args, Code: code, Msg: msg}
	}
	if v == nil {
		return nil
	}
	return json.Unmarshal([]byte(stdout), v)
}

// ---------------------------------------------------------------------------
// Repo / PR / status reads
// ---------------------------------------------------------------------------

type RepoInfo struct {
	NameWithOwner string `json:"nameWithOwner"`
	DefaultBranch string `json:"defaultBranch"`
	CurrentBranch string `json:"currentBranch"`
	HeadSHA       string `json:"headSha"`
}

func readRepo(ctx context.Context, dir string) (RepoInfo, error) {
	var info RepoInfo

	var view struct {
		NameWithOwner    string `json:"nameWithOwner"`
		DefaultBranchRef struct {
			Name string `json:"name"`
		} `json:"defaultBranchRef"`
	}
	if err := ghJSONIn(ctx, dir, &view, "repo", "view", "--json", "nameWithOwner,defaultBranchRef"); err != nil {
		return info, err
	}
	info.NameWithOwner = view.NameWithOwner
	info.DefaultBranch = view.DefaultBranchRef.Name

	if out, _, code, err := runIn(ctx, dir, "git", "rev-parse", "--abbrev-ref", "HEAD"); err == nil && code == 0 {
		info.CurrentBranch = out
	}
	if out, _, code, err := runIn(ctx, dir, "git", "rev-parse", "HEAD"); err == nil && code == 0 {
		info.HeadSHA = out
	}
	return info, nil
}

type PRInfo struct {
	Number    int       `json:"number"`
	Title     string    `json:"title"`
	Author    string    `json:"author"`
	URL       string    `json:"url"`
	State     string    `json:"state"`
	HeadSHA   string    `json:"headSha"`
	HeadRef   string    `json:"headRef"`
	BaseRef   string    `json:"baseRef"`
	Mergeable string    `json:"mergeable"`
	IsDraft   bool      `json:"isDraft"`
	UpdatedAt time.Time `json:"updatedAt"`
}

const pullRequestLimit = 30

const pullRequestJSONFields = "number,title,url,state,author,headRefOid,headRefName,baseRefName,mergeable,isDraft,updatedAt"

type rawPR struct {
	Number int    `json:"number"`
	Title  string `json:"title"`
	URL    string `json:"url"`
	State  string `json:"state"`
	Author struct {
		Login string `json:"login"`
	} `json:"author"`
	HeadRefOid  string    `json:"headRefOid"`
	HeadRefName string    `json:"headRefName"`
	BaseRefName string    `json:"baseRefName"`
	Mergeable   string    `json:"mergeable"`
	IsDraft     bool      `json:"isDraft"`
	UpdatedAt   time.Time `json:"updatedAt"`
}

func (raw rawPR) toPRInfo() PRInfo {
	return PRInfo{
		Number: raw.Number, Title: raw.Title, Author: raw.Author.Login,
		URL: raw.URL, State: raw.State, HeadSHA: raw.HeadRefOid,
		HeadRef: raw.HeadRefName, BaseRef: raw.BaseRefName,
		Mergeable: raw.Mergeable, IsDraft: raw.IsDraft, UpdatedAt: raw.UpdatedAt,
	}
}

// readPR returns the PR for the current branch. A missing PR is an expected
// state, not an error: you are often on a branch before opening one.
func readPR(ctx context.Context, dir string) (*PRInfo, error) {
	var raw rawPR
	err := ghJSONIn(ctx, dir, &raw, "pr", "view", "--json", pullRequestJSONFields)
	if err != nil {
		if notFound(err) || strings.Contains(err.Error(), "no pull requests found") ||
			strings.Contains(err.Error(), "no open pull requests") {
			return nil, nil
		}
		return nil, err
	}
	info := raw.toPRInfo()
	return &info, nil
}

// readPullRequests returns the open pull requests Signoff should track for a
// repository. "mine" follows the original All my PRs intent; "all" is useful
// for small teams sharing one workstation context.
func readPullRequests(ctx context.Context, dir, scope string) ([]PRInfo, bool, error) {
	var raw []rawPR
	args := []string{"pr", "list", "--state", "open", "--limit", fmt.Sprintf("%d", pullRequestLimit+1)}
	if scope != "all" {
		args = append(args, "--author", "@me")
	}
	args = append(args, "--json", pullRequestJSONFields)
	if err := ghJSONIn(ctx, dir, &raw, args...); err != nil {
		return nil, false, err
	}
	truncated := false
	raw, truncated = limitPullRequests(raw)
	out := make([]PRInfo, 0, len(raw))
	for _, pr := range raw {
		out = append(out, pr.toPRInfo())
	}
	return out, truncated, nil
}

func limitPullRequests(raw []rawPR) ([]rawPR, bool) {
	if len(raw) <= pullRequestLimit {
		return raw, false
	}
	return raw[:pullRequestLimit], true
}

// CommitStatus is one entry from the combined status API.
type CommitStatus struct {
	Context     string    `json:"context"`
	State       string    `json:"state"`
	Description string    `json:"description"`
	Creator     string    `json:"creator"`
	AvatarURL   string    `json:"avatarUrl"`
	CreatedAt   time.Time `json:"createdAt"`
	TargetURL   string    `json:"targetUrl"`
}

func readCommitStatuses(ctx context.Context, dir, repo, sha string) ([]CommitStatus, error) {
	var raw struct {
		Statuses []struct {
			Context     string    `json:"context"`
			State       string    `json:"state"`
			Description string    `json:"description"`
			CreatedAt   time.Time `json:"created_at"`
			TargetURL   string    `json:"target_url"`
			Creator     struct {
				Login     string `json:"login"`
				AvatarURL string `json:"avatar_url"`
			} `json:"creator"`
		} `json:"statuses"`
	}
	path := fmt.Sprintf("repos/%s/commits/%s/status", repo, sha)
	if err := ghJSONIn(ctx, dir, &raw, "api", path); err != nil {
		return nil, err
	}

	out := make([]CommitStatus, 0, len(raw.Statuses))
	for _, s := range raw.Statuses {
		out = append(out, CommitStatus{
			Context:     s.Context,
			State:       s.State,
			Description: s.Description,
			Creator:     s.Creator.Login,
			AvatarURL:   s.Creator.AvatarURL,
			CreatedAt:   s.CreatedAt,
			TargetURL:   s.TargetURL,
		})
	}
	return out, nil
}

// ProtectionStatus distinguishes the three outcomes of reading branch
// protection, which GitHub unhelpfully reports with the same 404 status code:
//
//	{"message":"Branch not protected"} -> the branch genuinely has no rules
//	{"message":"Not Found"}            -> the caller lacks admin, or it is gone
//
// Collapsing these two would either tell a repo owner their permissions are
// wrong, or tell a contributor that nothing is required when something is.
type ProtectionStatus int

const (
	ProtectionOK ProtectionStatus = iota
	ProtectionAbsent
	ProtectionUnreadable
)

// readProtectionContexts returns the required status check contexts for a
// branch, plus which of the three outcomes above occurred.
func readProtectionContexts(ctx context.Context, dir, repo, branch string) ([]string, ProtectionStatus) {
	var contexts []string
	path := fmt.Sprintf("repos/%s/branches/%s/protection", repo, branch)
	err := ghJSONIn(ctx, dir, &contexts, "api", path,
		"--jq", "[.required_status_checks.contexts[]?]")
	if err == nil {
		return contexts, ProtectionOK
	}

	var ge *ghError
	if errors.As(err, &ge) && strings.Contains(ge.Msg, "Branch not protected") {
		return nil, ProtectionAbsent
	}
	return nil, ProtectionUnreadable
}
