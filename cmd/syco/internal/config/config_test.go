package config_test

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/sycophant-md/sycophant/cmd/syco/internal/config"
)

func TestResolveConfig_MissingServer_ReturnsError(t *testing.T) {
	t.Setenv("SYCO_PROFILE", "")
	t.Setenv("SYCO_SERVER", "")
	t.Setenv("SYCO_AGENT", "")
	dir := t.TempDir()
	cfgFile := filepath.Join(dir, "config.toml")
	os.WriteFile(cfgFile, []byte(`[default]
profile = "dev"

[profile.dev]
agent = "hello-world"
`), 0644)

	_, err := config.LoadProfile(dir, "")
	if err == nil {
		t.Fatal("expected error for missing server, got nil")
	}
	if !strings.Contains(err.Error(), "server") {
		t.Fatalf("error should mention 'server', got: %s", err)
	}
}

func TestResolveConfig_ReturnsProfileNameAndValues(t *testing.T) {
	t.Setenv("SYCO_PROFILE", "")
	t.Setenv("SYCO_SERVER", "")
	t.Setenv("SYCO_AGENT", "")
	dir := t.TempDir()
	cfgFile := filepath.Join(dir, "config.toml")
	os.WriteFile(cfgFile, []byte(`[default]
profile = "staging"

[profile.staging]
server = "relay.staging.ts.local:9090"
agent = "my-agent"
`), 0644)

	p, err := config.LoadProfile(dir, "")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if p.Name != "staging" {
		t.Errorf("expected profile name 'staging', got %q", p.Name)
	}
	if p.Server != "relay.staging.ts.local:9090" {
		t.Errorf("expected server, got %q", p.Server)
	}
	if p.Agent != "my-agent" {
		t.Errorf("expected agent, got %q", p.Agent)
	}
}

func TestSetAgent_WritesConfig(t *testing.T) {
	t.Setenv("SYCO_PROFILE", "")
	t.Setenv("SYCO_SERVER", "")
	t.Setenv("SYCO_AGENT", "")
	dir := t.TempDir()
	cfgFile := filepath.Join(dir, "config.toml")
	os.WriteFile(cfgFile, []byte(`[default]
profile = "dev"

[profile.dev]
server = "relay.dev:9090"
`), 0644)

	err := config.SetAgent(dir, "", "new-agent")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	p, err := config.LoadProfile(dir, "")
	if err != nil {
		t.Fatalf("unexpected error loading after set: %v", err)
	}
	if p.Agent != "new-agent" {
		t.Errorf("expected agent 'new-agent', got %q", p.Agent)
	}
}

func TestListProfiles_ReturnsAllNames(t *testing.T) {
	dir := t.TempDir()
	cfgFile := filepath.Join(dir, "config.toml")
	os.WriteFile(cfgFile, []byte(`[default]
profile = "dev"

[profile.dev]
server = "a:1"

[profile.staging]
server = "b:2"

[profile.prod]
server = "c:3"
`), 0644)

	names, err := config.ListProfiles(dir)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(names) != 3 {
		t.Fatalf("expected 3 profiles, got %d: %v", len(names), names)
	}
	want := map[string]bool{"dev": true, "staging": true, "prod": true}
	for _, n := range names {
		if !want[n] {
			t.Errorf("unexpected profile %q", n)
		}
	}
}

func TestUseProfile_SetsDefault(t *testing.T) {
	t.Setenv("SYCO_PROFILE", "")
	t.Setenv("SYCO_SERVER", "")
	t.Setenv("SYCO_AGENT", "")
	dir := t.TempDir()
	cfgFile := filepath.Join(dir, "config.toml")
	os.WriteFile(cfgFile, []byte(`[default]
profile = "dev"

[profile.dev]
server = "a:1"

[profile.staging]
server = "b:2"
`), 0644)

	err := config.UseProfile(dir, "staging")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	p, err := config.LoadProfile(dir, "")
	if err != nil {
		t.Fatalf("unexpected error loading: %v", err)
	}
	if p.Name != "staging" {
		t.Errorf("expected active profile 'staging', got %q", p.Name)
	}
}

func TestLoadToken_Expired_ReturnsError(t *testing.T) {
	dir := t.TempDir()
	// Create a credentials.toml with an expired JWT.
	// The JWT payload has exp in the past. This is a minimal unsigned JWT
	// (header.payload.signature) with exp = 0 (1970-01-01).
	// Header: {"alg":"none","typ":"JWT"}
	// Payload: {"sub":"user@test","exp":0}
	expiredToken := "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJzdWIiOiJ1c2VyQHRlc3QiLCJleHAiOjB9."
	credFile := filepath.Join(dir, "credentials.toml")
	os.WriteFile(credFile, []byte(`[dev]
token = "`+expiredToken+`"
`), 0644)

	_, err := config.LoadToken(dir, "dev")
	if err == nil {
		t.Fatal("expected error for expired token, got nil")
	}
	if !strings.Contains(err.Error(), "session expired") {
		t.Fatalf("error should mention 'session expired', got: %s", err)
	}
}

func TestDeleteToken_RemovesEntry(t *testing.T) {
	dir := t.TempDir()
	credFile := filepath.Join(dir, "credentials.toml")
	// A token with exp far in the future.
	validToken := "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJzdWIiOiJ1c2VyQHRlc3QiLCJleHAiOjk5OTk5OTk5OTl9."
	os.WriteFile(credFile, []byte(`[dev]
token = "`+validToken+`"
`), 0644)

	err := config.DeleteToken(dir, "dev")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	// After deletion, loading should fail (no token).
	_, err = config.LoadToken(dir, "dev")
	if err == nil {
		t.Fatal("expected error after deleting token, got nil")
	}
}

// credentials.toml holds bearer tokens: every write leaves it readable by
// its owner only, even when it was world-readable before.
func TestTokenWrites_LeaveCredentialsOwnerOnly(t *testing.T) {
	dir := t.TempDir()
	credFile := filepath.Join(dir, "credentials.toml")
	for _, write := range []func() error{
		func() error { return config.SaveToken(dir, "dev", "T") },
		func() error { return config.DeleteToken(dir, "dev") },
	} {
		if err := os.WriteFile(credFile, []byte("[dev]\ntoken = \"old\"\n"), 0o644); err != nil {
			t.Fatal(err)
		}
		if err := os.Chmod(credFile, 0o644); err != nil {
			t.Fatal(err)
		}
		if err := write(); err != nil {
			t.Fatalf("unexpected error: %v", err)
		}
		info, err := os.Stat(credFile)
		if err != nil {
			t.Fatal(err)
		}
		if mode := info.Mode().Perm(); mode != 0o600 {
			t.Errorf("credentials.toml mode %o, want 600", mode)
		}
	}
}

// A damaged credentials.toml is an error, not a reason to drop every other
// profile's token.
func TestTokenWrites_DamagedCredentials_ReturnError(t *testing.T) {
	dir := t.TempDir()
	credFile := filepath.Join(dir, "credentials.toml")
	damaged := "[a]\ntoken = \"A\"\n[b\n"
	for name, write := range map[string]func() error{
		"SaveToken":   func() error { return config.SaveToken(dir, "c", "C") },
		"DeleteToken": func() error { return config.DeleteToken(dir, "c") },
	} {
		if err := os.WriteFile(credFile, []byte(damaged), 0o600); err != nil {
			t.Fatal(err)
		}
		if err := write(); err == nil {
			t.Errorf("%s on a damaged file returned nil, want an error", name)
		}
		if data, _ := os.ReadFile(credFile); string(data) != damaged {
			t.Errorf("%s rewrote the damaged file: %q", name, data)
		}

		// A file that can't be read: a symlink to itself.
		if err := os.Remove(credFile); err != nil {
			t.Fatal(err)
		}
		if err := os.Symlink("credentials.toml", credFile); err != nil {
			t.Fatal(err)
		}
		if err := write(); err == nil {
			t.Errorf("%s on an unreadable file returned nil, want an error", name)
		}
		if target, _ := os.Readlink(credFile); target != "credentials.toml" {
			t.Errorf("%s replaced the unreadable file", name)
		}
		if err := os.Remove(credFile); err != nil {
			t.Fatal(err)
		}
	}
}
