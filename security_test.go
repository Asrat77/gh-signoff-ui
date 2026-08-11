package main

import (
	"strings"
	"testing"
)

func TestRenderedIndexUsesPerResponseScriptNonce(t *testing.T) {
	first, err := renderIndex("test-token", false)
	if err != nil {
		t.Fatal(err)
	}
	second, err := renderIndex("test-token", false)
	if err != nil {
		t.Fatal(err)
	}
	for _, page := range [][]byte{first, second} {
		text := string(page)
		if strings.Contains(text, "{{NONCE}}") || strings.Contains(text, "script-src 'self' 'unsafe-inline'") {
			t.Fatalf("rendered page retained an unsafe or unresolved script policy: %s", text)
		}
		if !strings.Contains(text, "script-src 'self' 'nonce-") || !strings.Contains(text, `<script nonce="`) {
			t.Fatalf("rendered page did not include a nonce-backed script policy")
		}
	}
	if string(first) == string(second) {
		t.Fatal("separate responses should receive different script nonces")
	}
}
