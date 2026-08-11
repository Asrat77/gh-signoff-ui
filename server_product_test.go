package main

import (
	"bytes"
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"
)

func newDemoServerForTest(t *testing.T) (*server, *httptest.Server) {
	t.Helper()
	config, err := newConfigStore(filepath.Join(t.TempDir(), "config.json"))
	if err != nil {
		t.Fatal(err)
	}
	history, err := newHistoryStore(defaultHistoryPath(config.Path()))
	if err != nil {
		t.Fatal(err)
	}
	product := newProductService(config, history, t.TempDir(), true)
	product.Refresh(context.Background())
	srv := &server{
		token: "test-token", demo: newDemoStore(), config: config,
		history: history, product: product, running: map[string]bool{},
	}
	return srv, httptest.NewServer(srv.routes())
}

func TestProductAPIRequiresToken(t *testing.T) {
	_, ts := newDemoServerForTest(t)
	defer ts.Close()
	res, err := http.Get(ts.URL + "/api/product")
	if err != nil {
		t.Fatal(err)
	}
	defer res.Body.Close()
	if res.StatusCode != http.StatusForbidden {
		t.Fatalf("status = %d, want 403", res.StatusCode)
	}
}

func TestProductAPIRejectsTokenInURLAndForeignOrigins(t *testing.T) {
	srv, ts := newDemoServerForTest(t)
	defer ts.Close()

	queryResponse, err := http.Get(ts.URL + "/api/product?t=" + srv.token)
	if err != nil {
		t.Fatal(err)
	}
	_ = queryResponse.Body.Close()
	if queryResponse.StatusCode != http.StatusForbidden {
		t.Fatalf("query-string token status = %d, want 403", queryResponse.StatusCode)
	}

	for _, origin := range []string{"https://example.com", "http://localhost.example.com:3000"} {
		req, _ := http.NewRequest(http.MethodGet, ts.URL+"/api/product", nil)
		req.Header.Set("X-Signoff-Token", srv.token)
		req.Header.Set("Origin", origin)
		res, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		_ = res.Body.Close()
		if res.StatusCode != http.StatusForbidden {
			t.Fatalf("origin %q status = %d, want 403", origin, res.StatusCode)
		}
	}
}

func TestValidBranchNameRejectsLeadingDash(t *testing.T) {
	dir := t.TempDir()
	if validBranchName(context.Background(), dir, "--force") {
		t.Fatal("branch names beginning with a dash must be rejected")
	}
	if !validBranchName(context.Background(), dir, "feature/safe") {
		t.Fatal("valid branch name was rejected")
	}
}

func TestProductAPIAndDemoRunFlow(t *testing.T) {
	srv, ts := newDemoServerForTest(t)
	defer ts.Close()

	req, _ := http.NewRequest(http.MethodGet, ts.URL+"/api/product", nil)
	req.Header.Set("X-Signoff-Token", srv.token)
	res, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	body, _ := io.ReadAll(res.Body)
	_ = res.Body.Close()
	if res.StatusCode != http.StatusOK || !bytes.Contains(body, []byte(`"totalPullRequests":3`)) {
		t.Fatalf("unexpected product response (%d): %s", res.StatusCode, body)
	}

	runBody := strings.NewReader(`{"repoId":"demo-signoff","prNumber":482,"check":"security"}`)
	runReq, _ := http.NewRequest(http.MethodPost, ts.URL+"/api/run", runBody)
	runReq.Header.Set("Content-Type", "application/json")
	runReq.Header.Set("X-Signoff-Token", srv.token)
	runRes, err := http.DefaultClient.Do(runReq)
	if err != nil {
		t.Fatal(err)
	}
	stream, _ := io.ReadAll(runRes.Body)
	_ = runRes.Body.Close()
	if runRes.StatusCode != http.StatusOK || !bytes.Contains(stream, []byte(`event: done`)) || !bytes.Contains(stream, []byte(`{"code":0}`)) {
		t.Fatalf("unexpected run stream (%d): %s", runRes.StatusCode, stream)
	}

	pr, ok := srv.product.FindPullRequest("demo-signoff", 482)
	if !ok || !pr.Ready {
		t.Fatalf("demo PR should be ready after security passes: %+v", pr)
	}
}

func TestRepositoryUpdateDoesNotAllowPathReplacement(t *testing.T) {
	store := &ConfigStore{path: filepath.Join(t.TempDir(), "config.json"), cfg: defaultConfig()}
	store.cfg.Repositories = []TrackedRepository{{
		ID: "repo-1", Path: "/safe/path", NameWithOwner: "acme/repo", Enabled: true,
		Commands: map[string]CommandMapping{},
	}}
	updated, err := store.UpdateRepository("repo-1", TrackedRepository{
		Path: "/attacker/path", NameWithOwner: "other/repo", Enabled: true,
		PullRequestScope: "all", Commands: map[string]CommandMapping{"tests": {Command: "bin/test"}},
	})
	if err != nil {
		t.Fatal(err)
	}
	if updated.Path != "/safe/path" || updated.NameWithOwner != "acme/repo" {
		t.Fatalf("immutable repository identity changed: %+v", updated)
	}
}
