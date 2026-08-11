package main

import (
	"context"
	"os/exec"
	"runtime"
	"testing"
	"time"
)

func TestStreamProcessStopsDescendantProcessesOnTimeout(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("process groups are platform-specific")
	}
	if _, err := exec.LookPath("sh"); err != nil {
		t.Skip("sh is required for the process-tree test")
	}

	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()
	started := time.Now()
	code := streamProcess(ctx, t.TempDir(), "sh", []string{"-c", "sleep 30 & wait"}, func(string, string) {})
	if code != 124 {
		t.Fatalf("timeout exit code = %d, want 124", code)
	}
	if elapsed := time.Since(started); elapsed > 3*time.Second {
		t.Fatalf("timed-out process took %s to stop", elapsed)
	}
}
