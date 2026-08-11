package main

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestConfigStorePersistsRepositorySettings(t *testing.T) {
	path := filepath.Join(t.TempDir(), "config.json")
	store, err := newConfigStore(path)
	if err != nil {
		t.Fatal(err)
	}

	repo := TrackedRepository{
		ID:               "repo-1",
		Path:             "/tmp/example",
		NameWithOwner:    "acme/example",
		DefaultBranch:    "main",
		Enabled:          true,
		PullRequestScope: "all",
		Commands: map[string]CommandMapping{
			"tests": {Command: "bin/test", TimeoutSeconds: 90},
		},
	}
	store.cfg.Repositories = []TrackedRepository{repo}
	if err := store.saveLocked(); err != nil {
		t.Fatal(err)
	}

	reloaded, err := newConfigStore(path)
	if err != nil {
		t.Fatal(err)
	}
	got := reloaded.Snapshot()
	if len(got.Repositories) != 1 {
		t.Fatalf("repositories = %d, want 1", len(got.Repositories))
	}
	if got.Repositories[0].Commands["tests"].Command != "bin/test" {
		t.Fatalf("command was not persisted: %+v", got.Repositories[0].Commands)
	}
	if got.Repositories[0].PullRequestScope != "all" {
		t.Fatalf("scope = %q, want all", got.Repositories[0].PullRequestScope)
	}

	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm()&0o077 != 0 {
		t.Fatalf("config permissions = %o, want private", info.Mode().Perm())
	}
}

func TestConfigSnapshotIsDeepCopy(t *testing.T) {
	store := &ConfigStore{cfg: AppConfig{
		Repositories: []TrackedRepository{{ID: "one", Commands: map[string]CommandMapping{
			"tests": {Command: "bin/test"},
		}}},
	}}
	copy := store.Snapshot()
	copy.Repositories[0].Commands["tests"] = CommandMapping{Command: "changed"}
	if store.cfg.Repositories[0].Commands["tests"].Command != "bin/test" {
		t.Fatal("snapshot mutation changed the store")
	}
}

func TestCommandTimeoutIsBounded(t *testing.T) {
	repo := TrackedRepository{Commands: map[string]CommandMapping{
		"tests": {Command: " bin/test ", TimeoutSeconds: 100_000},
	}}
	normalizeTrackedRepository(&repo)
	if got := repo.Commands["tests"]; got.Command != "bin/test" || got.TimeoutSeconds != 7200 {
		t.Fatalf("normalized command = %+v, want trimmed command and 7200 second ceiling", got)
	}
}

func TestHistoryStorePersistsNewestFirst(t *testing.T) {
	path := filepath.Join(t.TempDir(), "history.json")
	history, err := newHistoryStore(path)
	if err != nil {
		t.Fatal(err)
	}
	old := time.Now().Add(-time.Hour)
	newer := time.Now()
	if err := history.Append(ActivityEvent{ID: "old", At: old}); err != nil {
		t.Fatal(err)
	}
	if err := history.Append(ActivityEvent{ID: "new", At: newer}); err != nil {
		t.Fatal(err)
	}

	reloaded, err := newHistoryStore(path)
	if err != nil {
		t.Fatal(err)
	}
	events := reloaded.Snapshot(10)
	if len(events) != 2 || events[0].ID != "new" || events[1].ID != "old" {
		t.Fatalf("unexpected history order: %+v", events)
	}
}
