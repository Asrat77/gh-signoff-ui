package main

import "testing"

func TestPullRequestLimitReportsTruncation(t *testing.T) {
	raw := make([]rawPR, pullRequestLimit+1)
	limited, truncated := limitPullRequests(raw)
	if !truncated {
		t.Fatal("expected a truncation signal")
	}
	if len(limited) != pullRequestLimit {
		t.Fatalf("limited pull requests = %d, want %d", len(limited), pullRequestLimit)
	}

	limited, truncated = limitPullRequests(make([]rawPR, pullRequestLimit))
	if truncated || len(limited) != pullRequestLimit {
		t.Fatalf("exact-limit pull requests = (%d, %t), want (%d, false)", len(limited), truncated, pullRequestLimit)
	}
}
