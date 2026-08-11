package main

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
)

const currentConfigVersion = 1

// CommandMapping is the explicit local work that must succeed before a
// signoff can be written. Signoff owns orchestration, not CI: the command is
// user-provided, runs in the tracked repository, and is never accepted from a
// run request.
type CommandMapping struct {
	Command        string `json:"command"`
	TimeoutSeconds int    `json:"timeoutSeconds"`
}

// TrackedRepository is one local checkout Signoff watches from the menu bar.
// ID is derived from the canonical path so it remains stable across launches.
type TrackedRepository struct {
	ID               string                    `json:"id"`
	Path             string                    `json:"path"`
	NameWithOwner    string                    `json:"nameWithOwner"`
	DefaultBranch    string                    `json:"defaultBranch"`
	Enabled          bool                      `json:"enabled"`
	PullRequestScope string                    `json:"pullRequestScope"` // mine | all
	Commands         map[string]CommandMapping `json:"commands"`
}

type AppConfig struct {
	Version        int                 `json:"version"`
	RefreshSeconds int                 `json:"refreshSeconds"`
	Repositories   []TrackedRepository `json:"repositories"`
}

type ConfigStore struct {
	path string
	mu   sync.RWMutex
	cfg  AppConfig
}

func defaultConfigPath() (string, error) {
	dir, err := os.UserConfigDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "Signoff", "config.json"), nil
}

func newConfigStore(path string) (*ConfigStore, error) {
	if path == "" {
		var err error
		path, err = defaultConfigPath()
		if err != nil {
			return nil, fmt.Errorf("resolving config directory: %w", err)
		}
	}

	s := &ConfigStore{path: path, cfg: defaultConfig()}
	b, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return s, nil
	}
	if err != nil {
		return nil, fmt.Errorf("reading config: %w", err)
	}
	if err := json.Unmarshal(b, &s.cfg); err != nil {
		return nil, fmt.Errorf("parsing config: %w", err)
	}
	s.cfg.normalize()
	return s, nil
}

func defaultConfig() AppConfig {
	return AppConfig{Version: currentConfigVersion, RefreshSeconds: 30, Repositories: []TrackedRepository{}}
}

func (c *AppConfig) normalize() {
	if c.Version == 0 {
		c.Version = currentConfigVersion
	}
	if c.RefreshSeconds < 10 {
		c.RefreshSeconds = 30
	}
	if c.Repositories == nil {
		c.Repositories = []TrackedRepository{}
	}
	for i := range c.Repositories {
		normalizeTrackedRepository(&c.Repositories[i])
	}
	sort.SliceStable(c.Repositories, func(i, j int) bool {
		return strings.ToLower(c.Repositories[i].NameWithOwner) < strings.ToLower(c.Repositories[j].NameWithOwner)
	})
}

func normalizeTrackedRepository(repo *TrackedRepository) {
	if repo.ID == "" && repo.Path != "" {
		repo.ID = repositoryID(repo.Path)
	}
	if repo.PullRequestScope != "all" {
		repo.PullRequestScope = "mine"
	}
	if repo.Commands == nil {
		repo.Commands = map[string]CommandMapping{}
	}
	for key, mapping := range repo.Commands {
		mapping.Command = strings.TrimSpace(mapping.Command)
		if mapping.TimeoutSeconds <= 0 {
			mapping.TimeoutSeconds = 1800
		} else if mapping.TimeoutSeconds > 7200 {
			mapping.TimeoutSeconds = 7200
		}
		repo.Commands[key] = mapping
	}
}

func repositoryID(path string) string {
	canonical := filepath.Clean(path)
	sum := sha256.Sum256([]byte(canonical))
	return hex.EncodeToString(sum[:8])
}

func (s *ConfigStore) Path() string { return s.path }

func (s *ConfigStore) Snapshot() AppConfig {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return cloneConfig(s.cfg)
}

func cloneConfig(in AppConfig) AppConfig {
	out := in
	out.Repositories = make([]TrackedRepository, len(in.Repositories))
	for i, repo := range in.Repositories {
		out.Repositories[i] = repo
		out.Repositories[i].Commands = make(map[string]CommandMapping, len(repo.Commands))
		for name, command := range repo.Commands {
			out.Repositories[i].Commands[name] = command
		}
	}
	return out
}

func (s *ConfigStore) AddRepository(ctx context.Context, path string) (TrackedRepository, error) {
	abs, err := filepath.Abs(strings.TrimSpace(path))
	if err != nil {
		return TrackedRepository{}, fmt.Errorf("resolving repository path: %w", err)
	}
	abs, err = filepath.EvalSymlinks(abs)
	if err != nil {
		return TrackedRepository{}, fmt.Errorf("opening repository path: %w", err)
	}
	if stat, err := os.Stat(abs); err != nil || !stat.IsDir() {
		return TrackedRepository{}, fmt.Errorf("repository path is not a directory")
	}
	if _, _, code, err := runIn(ctx, abs, "git", "rev-parse", "--git-dir"); err != nil || code != 0 {
		return TrackedRepository{}, fmt.Errorf("path is not a git repository")
	}

	info, err := readRepo(ctx, abs)
	if err != nil {
		return TrackedRepository{}, fmt.Errorf("reading GitHub repository: %w", err)
	}
	repo := TrackedRepository{
		ID:               repositoryID(abs),
		Path:             abs,
		NameWithOwner:    info.NameWithOwner,
		DefaultBranch:    info.DefaultBranch,
		Enabled:          true,
		PullRequestScope: "mine",
		Commands:         map[string]CommandMapping{},
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	for _, existing := range s.cfg.Repositories {
		if existing.ID == repo.ID {
			return TrackedRepository{}, fmt.Errorf("repository is already tracked")
		}
	}
	s.cfg.Repositories = append(s.cfg.Repositories, repo)
	s.cfg.normalize()
	if err := s.saveLocked(); err != nil {
		return TrackedRepository{}, err
	}
	return repo, nil
}

func (s *ConfigStore) UpdateRepository(id string, update TrackedRepository) (TrackedRepository, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for i := range s.cfg.Repositories {
		if s.cfg.Repositories[i].ID != id {
			continue
		}
		current := s.cfg.Repositories[i]
		current.Enabled = update.Enabled
		current.PullRequestScope = update.PullRequestScope
		current.Commands = update.Commands
		normalizeTrackedRepository(&current)
		s.cfg.Repositories[i] = current
		if err := s.saveLocked(); err != nil {
			return TrackedRepository{}, err
		}
		return current, nil
	}
	return TrackedRepository{}, os.ErrNotExist
}

func (s *ConfigStore) RemoveRepository(id string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	for i, repo := range s.cfg.Repositories {
		if repo.ID != id {
			continue
		}
		s.cfg.Repositories = append(s.cfg.Repositories[:i], s.cfg.Repositories[i+1:]...)
		return s.saveLocked()
	}
	return os.ErrNotExist
}

func (s *ConfigStore) SetRefreshSeconds(seconds int) error {
	if seconds < 10 || seconds > 3600 {
		return fmt.Errorf("refresh interval must be between 10 and 3600 seconds")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.cfg.RefreshSeconds = seconds
	return s.saveLocked()
}

func (s *ConfigStore) FindRepository(id string) (TrackedRepository, bool) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	for _, repo := range s.cfg.Repositories {
		if repo.ID == id {
			copy := repo
			copy.Commands = make(map[string]CommandMapping, len(repo.Commands))
			for name, command := range repo.Commands {
				copy.Commands[name] = command
			}
			return copy, true
		}
	}
	return TrackedRepository{}, false
}

func (s *ConfigStore) saveLocked() error {
	s.cfg.normalize()
	dir := filepath.Dir(s.path)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return fmt.Errorf("creating config directory: %w", err)
	}
	tmp, err := os.CreateTemp(dir, "config-*.json")
	if err != nil {
		return fmt.Errorf("creating config file: %w", err)
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)
	_ = tmp.Chmod(0o600)
	enc := json.NewEncoder(tmp)
	enc.SetIndent("", "  ")
	if err := enc.Encode(s.cfg); err != nil {
		_ = tmp.Close()
		return fmt.Errorf("writing config: %w", err)
	}
	if err := tmp.Close(); err != nil {
		return fmt.Errorf("closing config: %w", err)
	}
	if err := os.Rename(tmpName, s.path); err != nil {
		return fmt.Errorf("replacing config: %w", err)
	}
	return nil
}
