package main

import (
	"context"
	"fmt"
	"sort"
	"strings"
	"sync"
	"time"
)

type ProductSummary struct {
	TotalPullRequests int    `json:"totalPullRequests"`
	NeedsAttention    int    `json:"needsAttention"`
	Ready             int    `json:"ready"`
	Failing           int    `json:"failing"`
	Status            string `json:"status"` // neutral | ready | attention | failing
}

type RepositoryState struct {
	ID                    string                    `json:"id"`
	Path                  string                    `json:"path"`
	NameWithOwner         string                    `json:"nameWithOwner"`
	DefaultBranch         string                    `json:"defaultBranch"`
	CurrentBranch         string                    `json:"currentBranch"`
	CurrentSHA            string                    `json:"currentSha"`
	Enabled               bool                      `json:"enabled"`
	PullRequestScope      string                    `json:"pullRequestScope"`
	Commands              map[string]CommandMapping `json:"commands"`
	PullRequestCount      int                       `json:"pullRequestCount"`
	PullRequestsTruncated bool                      `json:"pullRequestsTruncated"`
	Error                 string                    `json:"error"`
}

type PullRequestState struct {
	ID         string    `json:"id"`
	RepoID     string    `json:"repoId"`
	Repo       string    `json:"repo"`
	RepoPath   string    `json:"repoPath"`
	PR         PRInfo    `json:"pr"`
	Checks     []Check   `json:"checks"`
	Rules      Rules     `json:"rules"`
	Passed     int       `json:"passed"`
	Total      int       `json:"total"`
	Ready      bool      `json:"ready"`
	Blocking   string    `json:"blocking"`
	Status     string    `json:"status"` // neutral | ready | attention | failing
	CheckedOut bool      `json:"checkedOut"`
	Warning    string    `json:"warning"`
	Error      string    `json:"error"`
	FetchedAt  time.Time `json:"fetchedAt"`
}

type ProductState struct {
	Env          Environment        `json:"env"`
	Summary      ProductSummary     `json:"summary"`
	Repositories []RepositoryState  `json:"repositories"`
	PullRequests []PullRequestState `json:"pullRequests"`
	History      []ActivityEvent    `json:"history"`
	ConfigPath   string             `json:"configPath"`
	Onboarded    bool               `json:"onboarded"`
	Demo         bool               `json:"demo"`
	Refreshing   bool               `json:"refreshing"`
	Warning      string             `json:"warning"`
	Error        string             `json:"error"`
	FetchedAt    time.Time          `json:"fetchedAt"`
}

type ProductService struct {
	config  *ConfigStore
	history *HistoryStore
	seedDir string
	demo    bool

	mu        sync.RWMutex
	state     ProductState
	refreshMu sync.Mutex
}

func newProductService(config *ConfigStore, history *HistoryStore, seedDir string, demo bool) *ProductService {
	return &ProductService{config: config, history: history, seedDir: seedDir, demo: demo}
}

func (p *ProductService) Snapshot(ctx context.Context) ProductState {
	p.mu.RLock()
	state := cloneProductState(p.state)
	p.mu.RUnlock()
	if state.FetchedAt.IsZero() {
		return p.Refresh(ctx)
	}
	return state
}

func (p *ProductService) Cached() ProductState {
	p.mu.RLock()
	defer p.mu.RUnlock()
	return cloneProductState(p.state)
}

func (p *ProductService) Refresh(ctx context.Context) ProductState {
	p.refreshMu.Lock()
	defer p.refreshMu.Unlock()

	p.mu.Lock()
	current := p.state
	current.Refreshing = true
	p.state = current
	p.mu.Unlock()

	var next ProductState
	if p.demo {
		next = buildDemoProductState()
	} else {
		next = buildProductState(ctx, p.config.Snapshot(), p.history.Snapshot(200), p.config.Path(), p.seedDir)
	}
	next.Refreshing = false
	next.normalize()
	p.mu.Lock()
	p.state = next
	p.mu.Unlock()
	return cloneProductState(next)
}

func (p *ProductService) FindPullRequest(repoID string, number int) (PullRequestState, bool) {
	p.mu.RLock()
	defer p.mu.RUnlock()
	for _, pr := range p.state.PullRequests {
		if pr.RepoID == repoID && pr.PR.Number == number {
			copy := pr
			copy.Checks = append([]Check{}, pr.Checks...)
			copy.Rules.Contexts = append([]string{}, pr.Rules.Contexts...)
			copy.Rules.SignoffContexts = append([]string{}, pr.Rules.SignoffContexts...)
			return copy, true
		}
	}
	return PullRequestState{}, false
}

func (p *ProductService) markDemoPassed(repoID string, number int, checkName string) {
	p.mu.Lock()
	defer p.mu.Unlock()
	for i := range p.state.PullRequests {
		pr := &p.state.PullRequests[i]
		if pr.RepoID != repoID || pr.PR.Number != number {
			continue
		}
		for j := range pr.Checks {
			if pr.Checks[j].Name == checkName {
				now := time.Now()
				pr.Checks[j].State = "success"
				pr.Checks[j].Actor = "you"
				pr.Checks[j].At = &now
			}
		}
		pr.Passed, pr.Total, pr.Blocking, pr.Ready = tally(pr.Checks)
		pr.Status = pullRequestStatus(*pr)
		p.state.History = append([]ActivityEvent{{
			ID: fmt.Sprintf("demo-run:%d", time.Now().UnixNano()), Kind: "run",
			RepoID: repoID, Repo: pr.Repo, PRNumber: number, PRTitle: pr.PR.Title,
			Check: checkName, State: "success", Actor: "you", At: time.Now(),
		}}, p.state.History...)
	}
	p.state.Summary = summarizePullRequests(p.state.PullRequests)
	p.state.FetchedAt = time.Now()
}

func buildProductState(ctx context.Context, cfg AppConfig, localHistory []ActivityEvent, configPath, seedDir string) ProductState {
	st := ProductState{
		ConfigPath: configPath,
		Onboarded:  len(cfg.Repositories) > 0,
		FetchedAt:  time.Now(),
	}
	probeDir := seedDir
	for _, repo := range cfg.Repositories {
		if repo.Enabled {
			probeDir = repo.Path
			break
		}
	}
	st.Env = probeEnvironment(ctx, probeDir)
	if !st.Env.GhInstalled || !st.Env.Authenticated {
		st.History = localHistory
		return st
	}

	type repoResult struct {
		index   int
		repo    RepositoryState
		prs     []PullRequestState
		history []ActivityEvent
	}
	results := make(chan repoResult, len(cfg.Repositories))
	var wg sync.WaitGroup
	sem := make(chan struct{}, 4)

	for i, tracked := range cfg.Repositories {
		wg.Add(1)
		go func(index int, repo TrackedRepository) {
			defer wg.Done()
			sem <- struct{}{}
			defer func() { <-sem }()
			view, prs, events := buildRepositoryState(ctx, repo)
			results <- repoResult{index: index, repo: view, prs: prs, history: events}
		}(i, tracked)
	}
	wg.Wait()
	close(results)

	ordered := make([]repoResult, 0, len(cfg.Repositories))
	for result := range results {
		ordered = append(ordered, result)
	}
	sort.Slice(ordered, func(i, j int) bool { return ordered[i].index < ordered[j].index })
	for _, result := range ordered {
		st.Repositories = append(st.Repositories, result.repo)
		st.PullRequests = append(st.PullRequests, result.prs...)
		st.History = append(st.History, result.history...)
	}
	st.History = append(st.History, localHistory...)
	sort.SliceStable(st.History, func(i, j int) bool { return st.History[i].At.After(st.History[j].At) })
	if len(st.History) > 300 {
		st.History = st.History[:300]
	}

	sort.SliceStable(st.PullRequests, func(i, j int) bool {
		rank := func(status string) int {
			switch status {
			case "failing":
				return 0
			case "attention":
				return 1
			case "neutral":
				return 2
			default:
				return 3
			}
		}
		if rank(st.PullRequests[i].Status) != rank(st.PullRequests[j].Status) {
			return rank(st.PullRequests[i].Status) < rank(st.PullRequests[j].Status)
		}
		return st.PullRequests[i].PR.UpdatedAt.After(st.PullRequests[j].PR.UpdatedAt)
	})
	st.Summary = summarizePullRequests(st.PullRequests)
	return st
}

func buildRepositoryState(ctx context.Context, tracked TrackedRepository) (RepositoryState, []PullRequestState, []ActivityEvent) {
	view := RepositoryState{
		ID: tracked.ID, Path: tracked.Path, NameWithOwner: tracked.NameWithOwner,
		DefaultBranch: tracked.DefaultBranch, Enabled: tracked.Enabled,
		PullRequestScope: tracked.PullRequestScope, Commands: tracked.Commands,
	}
	if !tracked.Enabled {
		return view, []PullRequestState{}, []ActivityEvent{}
	}
	info, err := readRepo(ctx, tracked.Path)
	if err != nil {
		view.Error = err.Error()
		return view, []PullRequestState{}, []ActivityEvent{}
	}
	view.NameWithOwner = info.NameWithOwner
	view.DefaultBranch = info.DefaultBranch
	view.CurrentBranch = info.CurrentBranch
	view.CurrentSHA = info.HeadSHA

	prs, truncated, err := readPullRequests(ctx, tracked.Path, tracked.PullRequestScope)
	if err != nil {
		view.Error = err.Error()
		return view, []PullRequestState{}, []ActivityEvent{}
	}
	view.PullRequestCount = len(prs)
	view.PullRequestsTruncated = truncated
	states := make([]PullRequestState, 0, len(prs))
	events := []ActivityEvent{}
	protection := map[string]struct {
		contexts []string
		status   ProtectionStatus
	}{}
	for _, pr := range prs {
		cached, ok := protection[pr.BaseRef]
		if !ok {
			contexts, status := readProtectionContexts(ctx, tracked.Path, info.NameWithOwner, pr.BaseRef)
			cached = struct {
				contexts []string
				status   ProtectionStatus
			}{contexts: contexts, status: status}
			protection[pr.BaseRef] = cached
		}
		state, history := buildPullRequestState(ctx, tracked, info, pr, cached.contexts, cached.status)
		states = append(states, state)
		events = append(events, history...)
	}
	return view, states, events
}

func buildPullRequestState(ctx context.Context, tracked TrackedRepository, repo RepoInfo, pr PRInfo, contexts []string, protection ProtectionStatus) (PullRequestState, []ActivityEvent) {
	st := PullRequestState{
		ID: fmt.Sprintf("%s:%d", tracked.ID, pr.Number), RepoID: tracked.ID,
		Repo: repo.NameWithOwner, RepoPath: tracked.Path, PR: pr,
		CheckedOut: repo.HeadSHA != "" && repo.HeadSHA == pr.HeadSHA, FetchedAt: time.Now(),
	}
	readable := protection != ProtectionUnreadable
	st.Rules = Rules{
		Branch: pr.BaseRef, Contexts: append([]string{}, contexts...), Readable: readable,
		Protected: protection == ProtectionOK,
	}
	for _, name := range contexts {
		if isSignoffContext(name) {
			st.Rules.SignoffContexts = append(st.Rules.SignoffContexts, name)
		}
	}
	st.Rules.Enforced = len(st.Rules.SignoffContexts) > 0
	st.Rules.InstallCommand = buildInstallCommand(st.Rules.SignoffContexts, pr.BaseRef)

	statuses, err := readCommitStatuses(ctx, tracked.Path, repo.NameWithOwner, pr.HeadSHA)
	if err != nil {
		st.Error = err.Error()
		st.Status = "failing"
		return st, []ActivityEvent{}
	}
	latest := map[string]CommitStatus{}
	for _, status := range statuses {
		if !isSignoffContext(status.Context) {
			continue
		}
		if previous, ok := latest[status.Context]; !ok || status.CreatedAt.After(previous.CreatedAt) {
			latest[status.Context] = status
		}
	}
	if !readable {
		st.Warning = "Branch protection is unreadable; readiness uses the default signoff assumption."
	}
	st.Checks = resolveChecks(st.Rules.SignoffContexts, latest, readable)
	for i := range st.Checks {
		if mapping, ok := tracked.Commands[st.Checks[i].Name]; ok && strings.TrimSpace(mapping.Command) != "" {
			st.Checks[i].LocalCommand = mapping.Command
			st.Checks[i].Configured = true
		}
	}
	st.Passed, st.Total, st.Blocking, st.Ready = tally(st.Checks)
	st.Status = pullRequestStatus(st)

	history := []ActivityEvent{}
	for _, status := range statuses {
		if !isSignoffContext(status.Context) {
			continue
		}
		history = append(history, ActivityEvent{
			ID:   fmt.Sprintf("github:%s:%d:%s:%s", tracked.ID, pr.Number, status.Context, status.CreatedAt.UTC().Format(time.RFC3339Nano)),
			Kind: "status", RepoID: tracked.ID, Repo: repo.NameWithOwner,
			PRNumber: pr.Number, PRTitle: pr.Title, Check: displayName(status.Context),
			State: status.State, Actor: actorFrom(status), Detail: status.Description, At: status.CreatedAt,
		})
	}
	return st, history
}

func pullRequestStatus(pr PullRequestState) string {
	if pr.Error != "" {
		return "failing"
	}
	for _, check := range pr.Checks {
		if check.Required && (check.State == "failure" || check.State == "error") {
			return "failing"
		}
	}
	if pr.Ready {
		return "ready"
	}
	if pr.Total > 0 {
		return "attention"
	}
	return "neutral"
}

func summarizePullRequests(prs []PullRequestState) ProductSummary {
	summary := ProductSummary{TotalPullRequests: len(prs), Status: "neutral"}
	for _, pr := range prs {
		switch pr.Status {
		case "failing":
			summary.Failing++
			summary.NeedsAttention++
		case "attention":
			summary.NeedsAttention++
		case "ready":
			summary.Ready++
		}
	}
	switch {
	case summary.Failing > 0:
		summary.Status = "failing"
	case summary.NeedsAttention > 0:
		summary.Status = "attention"
	case summary.TotalPullRequests > 0 && summary.Ready == summary.TotalPullRequests:
		summary.Status = "ready"
	}
	return summary
}

func (s *ProductState) normalize() {
	if s.Repositories == nil {
		s.Repositories = []RepositoryState{}
	}
	if s.PullRequests == nil {
		s.PullRequests = []PullRequestState{}
	}
	if s.History == nil {
		s.History = []ActivityEvent{}
	}
	for i := range s.PullRequests {
		if s.PullRequests[i].Checks == nil {
			s.PullRequests[i].Checks = []Check{}
		}
		if s.PullRequests[i].Rules.Contexts == nil {
			s.PullRequests[i].Rules.Contexts = []string{}
		}
		if s.PullRequests[i].Rules.SignoffContexts == nil {
			s.PullRequests[i].Rules.SignoffContexts = []string{}
		}
	}
}

func cloneProductState(in ProductState) ProductState {
	out := in
	out.Repositories = make([]RepositoryState, len(in.Repositories))
	for i, repo := range in.Repositories {
		out.Repositories[i] = repo
		out.Repositories[i].Commands = make(map[string]CommandMapping, len(repo.Commands))
		for name, command := range repo.Commands {
			out.Repositories[i].Commands[name] = command
		}
	}
	out.PullRequests = make([]PullRequestState, len(in.PullRequests))
	for i, pr := range in.PullRequests {
		out.PullRequests[i] = pr
		out.PullRequests[i].Checks = append([]Check{}, pr.Checks...)
		out.PullRequests[i].Rules.Contexts = append([]string{}, pr.Rules.Contexts...)
		out.PullRequests[i].Rules.SignoffContexts = append([]string{}, pr.Rules.SignoffContexts...)
	}
	out.History = append([]ActivityEvent{}, in.History...)
	return out
}

func buildDemoProductState() ProductState {
	now := time.Now()
	check := func(context, name, category, state, local string) Check {
		return Check{Context: context, Name: name, Category: category, State: state,
			Required: true, Command: signoffCommand(context), LocalCommand: local, Configured: local != ""}
	}
	repos := []RepositoryState{
		{ID: "demo-signoff", Path: "/demo/gh-signoff", NameWithOwner: "basecamp/gh-signoff", DefaultBranch: "main", CurrentBranch: "feature/partial-checks", CurrentSHA: "3db401c", Enabled: true, PullRequestScope: "mine", PullRequestCount: 2, Commands: map[string]CommandMapping{"tests": {Command: "bin/test"}, "lint": {Command: "bin/lint"}, "security": {Command: "bin/security"}}},
		{ID: "demo-rails", Path: "/demo/rails", NameWithOwner: "rails/rails", DefaultBranch: "main", Enabled: true, PullRequestScope: "mine", PullRequestCount: 1, Commands: map[string]CommandMapping{"tests": {Command: "bin/test"}}},
	}
	firstChecks := []Check{
		check("signoff/tests", "tests", "tests", "success", "bin/test"),
		check("signoff/lint", "lint", "lint", "success", "bin/lint"),
		check("signoff/security", "security", "security", "absent", "bin/security"),
	}
	prs := []PullRequestState{
		{ID: "demo-signoff:482", RepoID: "demo-signoff", Repo: "basecamp/gh-signoff", RepoPath: "/demo/gh-signoff", PR: PRInfo{Number: 482, Title: "Add label-based partial signoff", Author: "you", URL: "https://github.com/basecamp/gh-signoff/pull/482", State: "OPEN", HeadSHA: "3db401c", HeadRef: "feature/partial-checks", BaseRef: "main", Mergeable: "MERGEABLE", UpdatedAt: now.Add(-2 * time.Minute)}, Checks: firstChecks, Rules: Rules{Branch: "main", Contexts: []string{"signoff/tests", "signoff/lint", "signoff/security"}, SignoffContexts: []string{"signoff/tests", "signoff/lint", "signoff/security"}, Readable: true, Protected: true, Enforced: true}, Passed: 2, Total: 3, Blocking: "security", Status: "attention", CheckedOut: true, FetchedAt: now},
		{ID: "demo-signoff:485", RepoID: "demo-signoff", Repo: "basecamp/gh-signoff", RepoPath: "/demo/gh-signoff", PR: PRInfo{Number: 485, Title: "Fix race condition in poller", Author: "you", URL: "https://github.com/basecamp/gh-signoff/pull/485", State: "OPEN", HeadSHA: "9f3a201", HeadRef: "bug/race-fix", BaseRef: "main", Mergeable: "CONFLICTING", UpdatedAt: now.Add(-18 * time.Minute)}, Checks: []Check{check("signoff/tests", "tests", "tests", "failure", "bin/test"), check("signoff/lint", "lint", "lint", "absent", "bin/lint")}, Rules: Rules{Branch: "main", Contexts: []string{"signoff/tests", "signoff/lint"}, SignoffContexts: []string{"signoff/tests", "signoff/lint"}, Readable: true, Protected: true, Enforced: true}, Passed: 0, Total: 2, Blocking: "tests", Status: "failing", FetchedAt: now},
		{ID: "demo-rails:50122", RepoID: "demo-rails", Repo: "rails/rails", RepoPath: "/demo/rails", PR: PRInfo{Number: 50122, Title: "Bump version to 7.1.3", Author: "you", URL: "https://github.com/rails/rails/pull/50122", State: "OPEN", HeadSHA: "9b110d2", HeadRef: "version-bump", BaseRef: "main", Mergeable: "MERGEABLE", UpdatedAt: now.Add(-30 * time.Minute)}, Checks: []Check{check("signoff/tests", "tests", "tests", "success", "bin/test")}, Rules: Rules{Branch: "main", Contexts: []string{"signoff/tests"}, SignoffContexts: []string{"signoff/tests"}, Readable: true, Protected: true, Enforced: true}, Passed: 1, Total: 1, Ready: true, Status: "ready", FetchedAt: now},
	}
	history := []ActivityEvent{
		{ID: "demo-1", Kind: "status", RepoID: "demo-signoff", Repo: "basecamp/gh-signoff", PRNumber: 482, PRTitle: prs[0].PR.Title, Check: "lint", State: "success", Actor: "you", At: now.Add(-4 * time.Minute)},
		{ID: "demo-2", Kind: "run", RepoID: "demo-signoff", Repo: "basecamp/gh-signoff", PRNumber: 485, PRTitle: prs[1].PR.Title, Check: "tests", State: "failure", Command: "bin/test", ExitCode: 1, Detail: "1 failing test", At: now.Add(-22 * time.Minute)},
	}
	return ProductState{
		Env:     Environment{GhInstalled: true, GhVersion: "2.95.0", Authenticated: true, Login: "you", SignoffInstalled: true, SignoffVersion: "0.3.0"},
		Summary: summarizePullRequests(prs), Repositories: repos, PullRequests: prs,
		History: history, ConfigPath: "/demo/Signoff/config.json", Onboarded: true,
		Demo: true, FetchedAt: now,
	}
}
