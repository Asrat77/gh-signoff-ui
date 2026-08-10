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
	"os/exec"
	"strings"
	"sync"
	"time"
)

type server struct {
	dir      string
	token    string
	readOnly bool
	demo     *demoStore // non-nil only when -demo was passed

	mu      sync.Mutex
	running bool // one signoff at a time; the CLI is not reentrant per commit
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
	if tok == "" {
		tok = r.URL.Query().Get("t")
	}
	if subtle.ConstantTimeCompare([]byte(tok), []byte(s.token)) != 1 {
		return false
	}
	if origin := r.Header.Get("Origin"); origin != "" {
		if !strings.HasPrefix(origin, "http://127.0.0.1:") && !strings.HasPrefix(origin, "http://localhost:") {
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

	return mux
}

// handleSignoff streams a real `gh signoff [context]` run to the browser.
//
// This is the only route that writes anything. It runs the actual extension
// rather than POSTing a commit status directly, so all of gh-signoff's own
// safety checks (clean tree, commit pushed to a remote) still apply.
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
	if s.running {
		s.mu.Unlock()
		send("line", "another signoff is already running")
		send("done", `{"code":1}`)
		return
	}
	s.running = true
	s.mu.Unlock()
	defer func() {
		s.mu.Lock()
		s.running = false
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

func pick(cond bool, a, b string) string {
	if cond {
		return a
	}
	return b
}

func validContextName(s string) bool {
	if len(s) > 64 {
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

// listen binds to loopback only. Port 0 lets the OS pick a free port so
// several repos can each have a window open.
func listen(port int) (net.Listener, error) {
	return net.Listen("tcp", fmt.Sprintf("127.0.0.1:%d", port))
}
