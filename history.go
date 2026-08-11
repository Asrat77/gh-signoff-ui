package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"sync"
	"time"
)

type ActivityEvent struct {
	ID             string    `json:"id"`
	Kind           string    `json:"kind"` // run | signoff | status
	RepoID         string    `json:"repoId"`
	Repo           string    `json:"repo"`
	PRNumber       int       `json:"prNumber"`
	PRTitle        string    `json:"prTitle"`
	Check          string    `json:"check"`
	State          string    `json:"state"`
	Actor          string    `json:"actor"`
	Command        string    `json:"command"`
	Detail         string    `json:"detail"`
	ExitCode       int       `json:"exitCode"`
	DurationMillis int64     `json:"durationMillis"`
	At             time.Time `json:"at"`
}

type HistoryStore struct {
	path   string
	mu     sync.RWMutex
	events []ActivityEvent
}

func defaultHistoryPath(configPath string) string {
	return filepath.Join(filepath.Dir(configPath), "history.json")
}

func newHistoryStore(path string) (*HistoryStore, error) {
	h := &HistoryStore{path: path, events: []ActivityEvent{}}
	b, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return h, nil
	}
	if err != nil {
		return nil, fmt.Errorf("reading history: %w", err)
	}
	if err := json.Unmarshal(b, &h.events); err != nil {
		return nil, fmt.Errorf("parsing history: %w", err)
	}
	h.sortAndTrim()
	return h, nil
}

func (h *HistoryStore) Snapshot(limit int) []ActivityEvent {
	h.mu.RLock()
	defer h.mu.RUnlock()
	if limit <= 0 || limit > len(h.events) {
		limit = len(h.events)
	}
	out := make([]ActivityEvent, limit)
	copy(out, h.events[:limit])
	return out
}

func (h *HistoryStore) Append(event ActivityEvent) error {
	h.mu.Lock()
	defer h.mu.Unlock()
	if event.At.IsZero() {
		event.At = time.Now()
	}
	if event.ID == "" {
		event.ID = fmt.Sprintf("%d-%s-%d-%s", event.At.UnixNano(), event.RepoID, event.PRNumber, event.Check)
	}
	h.events = append([]ActivityEvent{event}, h.events...)
	h.sortAndTrim()
	return h.saveLocked()
}

func (h *HistoryStore) sortAndTrim() {
	sort.SliceStable(h.events, func(i, j int) bool { return h.events[i].At.After(h.events[j].At) })
	if len(h.events) > 500 {
		h.events = h.events[:500]
	}
	if h.events == nil {
		h.events = []ActivityEvent{}
	}
}

func (h *HistoryStore) saveLocked() error {
	dir := filepath.Dir(h.path)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	tmp, err := os.CreateTemp(dir, "history-*.json")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)
	_ = tmp.Chmod(0o600)
	enc := json.NewEncoder(tmp)
	enc.SetIndent("", "  ")
	if err := enc.Encode(h.events); err != nil {
		_ = tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	return os.Rename(tmpName, h.path)
}
