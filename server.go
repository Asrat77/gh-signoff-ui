package main

import (
	"bufio"
	"context"
	"crypto/rand"
	"crypto/subtle"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"sort"
	"strings"
	"sync"
	"time"
)

type server struct {
	dir      string
	token    string
	readOnly bool
	demo     *demoStore // non-nil only when -demo was passed
	config   *ConfigStore
	history  *HistoryStore
	product  *ProductService

	mu      sync.Mutex
	running map[string]bool // one run per PR/check; gh-signoff is not reentrant per commit
}

func newToken() string {
	b := make([]byte, 24)
	if _, err := rand.Read(b); err != nil {
		log.Fatalf("generating token: %v", err)
	}
	return hex.EncodeToString(b)
}

// authorized gates every API route.
//
// Binding to loopback is not by itself sufficient: any web page the user has
// open can issue requests to 127.0.0.1. A per-run token plus an Origin check
// keeps a drive-by page from triggering a signoff on the user's behalf.
func (s *server) authorized(r *http.Request) bool {
	tok := r.Header.Get("X-Signoff-Token")
	if subtle.ConstantTimeCompare([]byte(tok), []byte(s.token)) != 1 {
		return false
	}
	if origin := r.Header.Get("Origin"); origin != "" {
		parsed, err := url.Parse(origin)
		if err != nil || parsed.Scheme != "http" || (parsed.Hostname() != "127.0.0.1" && parsed.Hostname() != "localhost") {
			return false
		}
	}
	return true
}

func (s *server) routes() *http.ServeMux {
	mux := http.NewServeMux()

	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/" {
			http.NotFound(w, r)
			return
		}
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		w.Header().Set("Cache-Control", "no-store")
		page, err := renderIndex(s.token, s.readOnly)
		if err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		_, _ = w.Write(page)
	})

	mux.HandleFunc("/app.css", serveAsset("text/css; charset=utf-8", "web/app.css"))
	mux.HandleFunc("/app.js", serveAsset("application/javascript; charset=utf-8", "web/app.js"))

	mux.HandleFunc("/api/state", func(w http.ResponseWriter, r *http.Request) {
		if !s.authorized(r) {
			http.Error(w, "forbidden", http.StatusForbidden)
			return
		}
		ctx, cancel := context.WithTimeout(r.Context(), 25*time.Second)
		defer cancel()

		var st State
		if s.demo != nil {
			st = s.demo.snapshot()
		} else {
			st = buildState(ctx, s.dir)
			st.normalize()
		}
		w.Header().Set("Content-Type", "application/json")
		w.Header().Set("Cache-Control", "no-store")
		_ = json.NewEncoder(w).Encode(st)
	})

	mux.HandleFunc("/api/signoff", s.handleSignoff)
	mux.HandleFunc("/api/product", s.handleProduct)
	mux.HandleFunc("/api/refresh", s.handleRefresh)
	mux.HandleFunc("/api/config", s.handleConfig)
	mux.HandleFunc("/api/repositories", s.handleRepositories)
	mux.HandleFunc("/api/repositories/", s.handleRepository)
	mux.HandleFunc("/api/run", s.handleRunAndSignoff)
	mux.HandleFunc("/api/rules", s.handleRules)

	return mux
}

// handleSignoff retains the original single-repository endpoint for CLI
// compatibility. The desktop product uses handleRunAndSignoff below.
func (s *server) handleSignoff(w http.ResponseWriter, r *http.Request) {
	if !s.authorized(r) {
		http.Error(w, "forbidden", http.StatusForbidden)
		return
	}
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}

	var body struct {
		Name string `json:"name"` // "" means the default signoff context
	}
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil && err != io.EOF {
		http.Error(w, "bad request", http.StatusBadRequest)
		return
	}

	// Guard against argument injection: a context name is a bare token.
	name := strings.TrimSpace(body.Name)
	if name != "" && !validContextName(name) {
		http.Error(w, "invalid check name", http.StatusBadRequest)
		return
	}

	flusher, ok := w.(http.Flusher)
	if !ok {
		http.Error(w, "streaming unsupported", http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-store")
	w.Header().Set("X-Accel-Buffering", "no")

	send := func(event, data string) {
		fmt.Fprintf(w, "event: %s\ndata: %s\n\n", event, strings.ReplaceAll(data, "\n", "\\n"))
		flusher.Flush()
	}

	args := []string{"signoff"}
	if name != "" {
		args = append(args, name)
	}
	pretty := "gh " + strings.Join(args, " ")
	send("line", "$ "+pretty)

	if s.demo != nil {
		for _, line := range []string{
			"running locally…",
			"✓ " + name + " passed",
		} {
			time.Sleep(450 * time.Millisecond)
			send("line", line)
		}
		s.demo.signoff(name)
		send("line", "✓ Signed off on 3db401c"+pick(name != "", " for "+name, ""))
		send("done", `{"code":0}`)
		return
	}

	if s.readOnly {
		send("line", "read-only mode: refusing to write a commit status")
		send("done", `{"code":1,"readOnly":true}`)
		return
	}

	s.mu.Lock()
	if s.running["legacy"] {
		s.mu.Unlock()
		send("line", "another signoff is already running")
		send("done", `{"code":1}`)
		return
	}
	s.running["legacy"] = true
	s.mu.Unlock()
	defer func() {
		s.mu.Lock()
		delete(s.running, "legacy")
		s.mu.Unlock()
	}()

	ctx, cancel := context.WithTimeout(r.Context(), 2*time.Minute)
	defer cancel()

	cmd := exec.CommandContext(ctx, "gh", args...)
	cmd.Dir = s.dir

	stdout, err := cmd.StdoutPipe()
	if err != nil {
		send("line", "could not capture output: "+err.Error())
		send("done", `{"code":1}`)
		return
	}
	// Same pipe for both streams so the terminal panel shows gh's errors
	// interleaved with its output, exactly as a shell would.
	cmd.Stderr = cmd.Stdout

	if err := cmd.Start(); err != nil {
		send("line", "could not start gh: "+err.Error())
		send("done", `{"code":1}`)
		return
	}

	scanner := bufio.NewScanner(stdout)
	scanner.Buffer(make([]byte, 0, 64*1024), 1024*1024)
	for scanner.Scan() {
		send("line", scanner.Text())
	}

	code := 0
	if err := cmd.Wait(); err != nil {
		if ee, ok := err.(*exec.ExitError); ok {
			code = ee.ExitCode()
		} else {
			code = 1
			send("line", err.Error())
		}
	}
	send("done", fmt.Sprintf(`{"code":%d}`, code))
}

func (s *server) handleProduct(w http.ResponseWriter, r *http.Request) {
	if !s.authorized(r) {
		http.Error(w, "forbidden", http.StatusForbidden)
		return
	}
	if r.Method != http.MethodGet {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 45*time.Second)
	defer cancel()
	writeJSON(w, http.StatusOK, s.product.Snapshot(ctx))
}

func (s *server) handleRefresh(w http.ResponseWriter, r *http.Request) {
	if !s.authorized(r) {
		http.Error(w, "forbidden", http.StatusForbidden)
		return
	}
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 45*time.Second)
	defer cancel()
	writeJSON(w, http.StatusOK, s.product.Refresh(ctx))
}

func (s *server) handleConfig(w http.ResponseWriter, r *http.Request) {
	if !s.authorized(r) {
		http.Error(w, "forbidden", http.StatusForbidden)
		return
	}
	switch r.Method {
	case http.MethodGet:
		writeJSON(w, http.StatusOK, s.config.Snapshot())
	case http.MethodPut:
		var body struct {
			RefreshSeconds int `json:"refreshSeconds"`
		}
		if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
			http.Error(w, "bad request", http.StatusBadRequest)
			return
		}
		if err := s.config.SetRefreshSeconds(body.RefreshSeconds); err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		writeJSON(w, http.StatusOK, s.config.Snapshot())
	default:
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
	}
}

func (s *server) handleRepositories(w http.ResponseWriter, r *http.Request) {
	if !s.authorized(r) {
		http.Error(w, "forbidden", http.StatusForbidden)
		return
	}
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	var body struct {
		Path string `json:"path"`
	}
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil || strings.TrimSpace(body.Path) == "" {
		http.Error(w, "repository path is required", http.StatusBadRequest)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 30*time.Second)
	defer cancel()
	repo, err := s.config.AddRepository(ctx, body.Path)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	go s.refreshInBackground()
	writeJSON(w, http.StatusCreated, repo)
}

func (s *server) handleRepository(w http.ResponseWriter, r *http.Request) {
	if !s.authorized(r) {
		http.Error(w, "forbidden", http.StatusForbidden)
		return
	}
	id := strings.TrimPrefix(r.URL.Path, "/api/repositories/")
	if id == "" || strings.Contains(id, "/") {
		http.Error(w, "invalid repository id", http.StatusBadRequest)
		return
	}
	switch r.Method {
	case http.MethodPut:
		var update TrackedRepository
		if err := json.NewDecoder(r.Body).Decode(&update); err != nil {
			http.Error(w, "bad request", http.StatusBadRequest)
			return
		}
		repo, err := s.config.UpdateRepository(id, update)
		if err != nil {
			status := http.StatusBadRequest
			if os.IsNotExist(err) {
				status = http.StatusNotFound
			}
			http.Error(w, err.Error(), status)
			return
		}
		go s.refreshInBackground()
		writeJSON(w, http.StatusOK, repo)
	case http.MethodDelete:
		if err := s.config.RemoveRepository(id); err != nil {
			status := http.StatusBadRequest
			if os.IsNotExist(err) {
				status = http.StatusNotFound
			}
			http.Error(w, err.Error(), status)
			return
		}
		go s.refreshInBackground()
		w.WriteHeader(http.StatusNoContent)
	default:
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
	}
}

func (s *server) handleRunAndSignoff(w http.ResponseWriter, r *http.Request) {
	if !s.authorized(r) {
		http.Error(w, "forbidden", http.StatusForbidden)
		return
	}
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	var body struct {
		RepoID   string `json:"repoId"`
		PRNumber int    `json:"prNumber"`
		Check    string `json:"check"`
	}
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		http.Error(w, "bad request", http.StatusBadRequest)
		return
	}
	body.Check = strings.TrimSpace(body.Check)
	if body.RepoID == "" || body.PRNumber <= 0 || !validContextName(body.Check) {
		http.Error(w, "invalid run target", http.StatusBadRequest)
		return
	}
	pr, ok := s.product.FindPullRequest(body.RepoID, body.PRNumber)
	if !ok {
		http.Error(w, "pull request is no longer tracked; refresh and try again", http.StatusConflict)
		return
	}
	var target Check
	found := false
	for _, check := range pr.Checks {
		if check.Name == body.Check {
			target, found = check, true
			break
		}
	}
	if !found {
		http.Error(w, "check is not present on this pull request", http.StatusBadRequest)
		return
	}
	if !target.Configured || strings.TrimSpace(target.LocalCommand) == "" {
		http.Error(w, "configure a local command for this check first", http.StatusConflict)
		return
	}
	if s.readOnly {
		http.Error(w, "read-only mode refuses local commands and GitHub writes", http.StatusForbidden)
		return
	}
	if s.demo == nil {
		repo, exists := s.config.FindRepository(body.RepoID)
		if !exists {
			http.Error(w, "tracked repository was removed", http.StatusConflict)
			return
		}
		if err := requireCheckedOutCommit(r.Context(), repo.Path, pr.PR.HeadSHA); err != nil {
			http.Error(w, err.Error(), http.StatusConflict)
			return
		}
	}

	flusher, ok := w.(http.Flusher)
	if !ok {
		http.Error(w, "streaming unsupported", http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-store")
	w.Header().Set("X-Accel-Buffering", "no")
	send := func(event, data string) {
		fmt.Fprintf(w, "event: %s\ndata: %s\n\n", event, strings.ReplaceAll(data, "\n", "\\n"))
		flusher.Flush()
	}

	key := fmt.Sprintf("%s:%d:%s", body.RepoID, body.PRNumber, body.Check)
	s.mu.Lock()
	if s.running[key] {
		s.mu.Unlock()
		send("done", `{"code":1,"error":"already running"}`)
		return
	}
	s.running[key] = true
	s.mu.Unlock()
	defer func() {
		s.mu.Lock()
		delete(s.running, key)
		s.mu.Unlock()
	}()

	if s.demo != nil {
		send("phase", "run")
		send("line", "$ "+target.LocalCommand)
		send("line", "✓ local command passed")
		send("phase", "signoff")
		send("line", "$ gh signoff --commit "+pr.PR.HeadSHA+" "+body.Check)
		send("line", "✓ signed off "+body.Check)
		s.product.markDemoPassed(body.RepoID, body.PRNumber, body.Check)
		send("done", `{"code":0}`)
		return
	}

	repo, ok := s.config.FindRepository(body.RepoID)
	if !ok {
		send("line", "tracked repository was removed")
		send("done", `{"code":1}`)
		return
	}
	mapping, ok := repo.Commands[body.Check]
	if !ok || strings.TrimSpace(mapping.Command) == "" {
		send("line", "local command mapping was removed")
		send("done", `{"code":1}`)
		return
	}
	if mapping.TimeoutSeconds <= 0 {
		mapping.TimeoutSeconds = 1800
	}

	started := time.Now()
	send("phase", "run")
	send("line", "$ "+mapping.Command)
	runCtx, cancelRun := context.WithTimeout(r.Context(), time.Duration(mapping.TimeoutSeconds)*time.Second)
	runCode := streamProcess(runCtx, repo.Path, "/bin/zsh", []string{"-lc", mapping.Command}, send)
	cancelRun()
	runState := "success"
	if runCode != 0 {
		runState = "failure"
	}
	_ = s.history.Append(ActivityEvent{
		Kind: "run", RepoID: repo.ID, Repo: pr.Repo, PRNumber: pr.PR.Number,
		PRTitle: pr.PR.Title, Check: body.Check, State: runState, Command: mapping.Command,
		ExitCode: runCode, DurationMillis: time.Since(started).Milliseconds(), At: time.Now(),
	})
	if runCode != 0 {
		send("done", fmt.Sprintf(`{"code":%d,"phase":"run"}`, runCode))
		return
	}
	if err := requireCheckedOutCommit(r.Context(), repo.Path, pr.PR.HeadSHA); err != nil {
		send("line", err.Error())
		send("done", `{"code":1,"phase":"checkout"}`)
		return
	}

	send("phase", "signoff")
	args := []string{"signoff", "--commit", pr.PR.HeadSHA}
	if body.Check != "signoff" {
		args = append(args, body.Check)
	}
	send("line", "$ gh "+strings.Join(args, " "))
	signoffCtx, cancelSignoff := context.WithTimeout(r.Context(), 2*time.Minute)
	signoffCode := streamProcess(signoffCtx, repo.Path, "gh", args, send)
	cancelSignoff()
	signoffState := "success"
	if signoffCode != 0 {
		signoffState = "failure"
	}
	_ = s.history.Append(ActivityEvent{
		Kind: "signoff", RepoID: repo.ID, Repo: pr.Repo, PRNumber: pr.PR.Number,
		PRTitle: pr.PR.Title, Check: body.Check, State: signoffState,
		Command: "gh " + strings.Join(args, " "), ExitCode: signoffCode, At: time.Now(),
	})
	if signoffCode == 0 {
		go s.refreshInBackground()
	}
	send("done", fmt.Sprintf(`{"code":%d,"phase":"signoff"}`, signoffCode))
}

func streamProcess(ctx context.Context, dir, name string, args []string, send func(string, string)) int {
	cmd := exec.CommandContext(ctx, name, args...)
	cmd.Dir = dir
	configureProcessGroup(cmd)
	cmd.Cancel = func() error { return cancelProcessGroup(cmd) }
	cmd.WaitDelay = 2 * time.Second
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		send("line", "could not capture output: "+err.Error())
		return 1
	}
	cmd.Stderr = cmd.Stdout
	if err := cmd.Start(); err != nil {
		send("line", "could not start command: "+err.Error())
		return 1
	}
	scanner := bufio.NewScanner(stdout)
	scanner.Buffer(make([]byte, 0, 64*1024), 1024*1024)
	for scanner.Scan() {
		send("line", scanner.Text())
	}
	if err := scanner.Err(); err != nil {
		send("line", "output stream failed: "+err.Error())
	}
	if err := cmd.Wait(); err != nil {
		if ctx.Err() == context.DeadlineExceeded {
			send("line", "command timed out")
			return 124
		}
		if exit, ok := err.(*exec.ExitError); ok {
			return exit.ExitCode()
		}
		send("line", err.Error())
		return 1
	}
	return 0
}

func (s *server) handleRules(w http.ResponseWriter, r *http.Request) {
	if !s.authorized(r) {
		http.Error(w, "forbidden", http.StatusForbidden)
		return
	}
	if r.Method != http.MethodPut {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	if s.readOnly {
		http.Error(w, "read-only mode refuses branch-protection writes", http.StatusForbidden)
		return
	}
	var body struct {
		RepoID string   `json:"repoId"`
		Branch string   `json:"branch"`
		Checks []string `json:"checks"`
	}
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		http.Error(w, "bad request", http.StatusBadRequest)
		return
	}
	repo, ok := s.config.FindRepository(body.RepoID)
	if !ok {
		http.Error(w, "repository not found", http.StatusNotFound)
		return
	}
	body.Branch = strings.TrimSpace(body.Branch)
	if !validBranchName(r.Context(), repo.Path, body.Branch) {
		http.Error(w, "invalid branch name", http.StatusBadRequest)
		return
	}
	seen := map[string]bool{}
	desired := []string{}
	for _, name := range body.Checks {
		name = strings.TrimSpace(name)
		if !validContextName(name) || seen[name] {
			http.Error(w, "invalid or duplicate check name", http.StatusBadRequest)
			return
		}
		seen[name] = true
		if name == "signoff" {
			desired = append(desired, "signoff")
		} else {
			desired = append(desired, "signoff/"+name)
		}
	}
	sort.Strings(desired)

	ctx, cancel := context.WithTimeout(r.Context(), 30*time.Second)
	defer cancel()
	info, err := readRepo(ctx, repo.Path)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadGateway)
		return
	}
	contexts, protection := readProtectionContexts(ctx, repo.Path, info.NameWithOwner, body.Branch)
	if protection == ProtectionUnreadable {
		http.Error(w, "branch protection is unreadable with the current GitHub permissions", http.StatusForbidden)
		return
	}
	combined := mergeSignoffContexts(contexts, desired)

	if protection == ProtectionAbsent {
		if len(desired) == 0 {
			writeJSON(w, http.StatusOK, map[string]any{"contexts": combined, "changed": false})
			return
		}
		args := []string{"signoff", "install"}
		for _, contextName := range desired {
			if contextName != "signoff" {
				args = append(args, strings.TrimPrefix(contextName, "signoff/"))
			}
		}
		args = append(args, "--branch", body.Branch)
		stdout, stderr, code, launchErr := runIn(ctx, repo.Path, "gh", args...)
		if launchErr != nil || code != 0 {
			http.Error(w, pick(stderr != "", stderr, stdout), http.StatusBadGateway)
			return
		}
	} else {
		branch := url.PathEscape(body.Branch)
		base := fmt.Sprintf("repos/%s/branches/%s/protection/required_status_checks", info.NameWithOwner, branch)
		var args []string
		var input string
		switch {
		case len(combined) == 0 && len(contexts) > 0:
			args = []string{"api", "--method", "DELETE", base}
		case len(contexts) == 0 && len(combined) > 0:
			payload, _ := json.Marshal(map[string]any{"strict": false, "contexts": combined})
			input = string(payload)
			args = []string{"api", "--method", "PATCH", base, "--input", "-"}
		case len(combined) > 0:
			payload, _ := json.Marshal(map[string]any{"contexts": combined})
			input = string(payload)
			args = []string{"api", "--method", "PUT", base + "/contexts", "--input", "-"}
		default:
			writeJSON(w, http.StatusOK, map[string]any{"contexts": combined, "changed": false})
			return
		}
		stdout, stderr, code, launchErr := runInWithInput(ctx, repo.Path, input, "gh", args...)
		if launchErr != nil || code != 0 {
			http.Error(w, pick(stderr != "", stderr, stdout), http.StatusBadGateway)
			return
		}
	}
	_ = s.history.Append(ActivityEvent{Kind: "rules", RepoID: repo.ID, Repo: info.NameWithOwner, Check: strings.Join(body.Checks, ", "), State: "success", Detail: "Updated required signoff checks on " + body.Branch, At: time.Now()})
	go s.refreshInBackground()
	writeJSON(w, http.StatusOK, map[string]any{"contexts": combined, "changed": true})
}

func (s *server) refreshInBackground() {
	ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
	defer cancel()
	s.product.Refresh(ctx)
}

func writeJSON(w http.ResponseWriter, status int, value any) {
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(value)
}

func pick(cond bool, a, b string) string {
	if cond {
		return a
	}
	return b
}

func validContextName(s string) bool {
	if s == "" || len(s) > 64 {
		return false
	}
	// A leading dash would make gh parse the name as an option rather than a
	// positional argument. gh-signoff's own -f overrides its check that the
	// commit is present on a remote, so this is not merely cosmetic.
	if s[0] == '-' {
		return false
	}
	for _, r := range s {
		switch {
		case r >= 'a' && r <= 'z', r >= 'A' && r <= 'Z', r >= '0' && r <= '9':
		case r == '-', r == '_', r == '.':
		default:
			return false
		}
	}
	return true
}

func validBranchName(ctx context.Context, dir, branch string) bool {
	if branch == "" || strings.HasPrefix(branch, "-") {
		return false
	}
	_, _, code, err := runIn(ctx, dir, "git", "check-ref-format", "--branch", branch)
	return err == nil && code == 0
}

func mergeSignoffContexts(existing, desired []string) []string {
	seen := map[string]bool{}
	combined := []string{}
	for _, contextName := range append(append([]string{}, existing...), desired...) {
		if isSignoffContext(contextName) && !contains(desired, contextName) {
			continue
		}
		if contextName == "" || seen[contextName] {
			continue
		}
		seen[contextName] = true
		combined = append(combined, contextName)
	}
	sort.Strings(combined)
	return combined
}

func contains(values []string, candidate string) bool {
	for _, value := range values {
		if value == candidate {
			return true
		}
	}
	return false
}

func requireCheckedOutCommit(ctx context.Context, dir, expected string) error {
	actual, stderr, code, err := runIn(ctx, dir, "git", "rev-parse", "HEAD")
	if err != nil || code != 0 {
		return fmt.Errorf("could not verify the checked-out commit: %s", pick(stderr != "", stderr, "git rev-parse failed"))
	}
	if actual != expected {
		return fmt.Errorf("this checkout is at %s, not pull request commit %s; check out the PR branch and refresh", shortSHA(actual), shortSHA(expected))
	}
	return nil
}

func shortSHA(sha string) string {
	if len(sha) > 8 {
		return sha[:8]
	}
	return sha
}

// listen binds to loopback only. Port 0 lets the OS pick a free port so
// several repos can each have a window open.
func listen(port int) (net.Listener, error) {
	return net.Listen("tcp", fmt.Sprintf("127.0.0.1:%d", port))
}
