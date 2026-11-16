package cmd

import (
	"bytes"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// runPrinting runs `syco args...` and returns what it printed.
func runPrinting(t *testing.T, args ...string) string {
	t.Helper()
	var out bytes.Buffer
	root := Root()
	root.SetArgs(args)
	root.SetOut(&out)
	root.SetErr(io.Discard)
	if err := root.Execute(); err != nil {
		t.Fatalf("syco %s returned %v", strings.Join(args, " "), err)
	}
	return out.String()
}

func TestSessionAlignsItsValues(t *testing.T) {
	f, _ := newClientEnv(t)

	got := runPrinting(t, "session")
	want := fmt.Sprintf("Profile:   test\nServer:    %s\nAgent:     %s\n", f.addr, testAgent)
	if got != want {
		t.Errorf("syco session printed %q, want %q", got, want)
	}
}

func TestWhoamiPrintsExpiryInUTC(t *testing.T) {
	_, dir := newClientEnv(t)

	exp := time.Now().Add(time.Hour).Truncate(time.Second)
	creds := fmt.Sprintf("[test]\ntoken = %q\n", futureToken(t, exp))
	if err := os.WriteFile(filepath.Join(dir, "credentials.toml"), []byte(creds), 0o600); err != nil {
		t.Fatal(err)
	}

	got := runPrinting(t, "whoami")
	want := "Expires: " + exp.UTC().Format("2006-01-02 15:04:05") + " UTC\n"
	if !strings.Contains(got, want) {
		t.Errorf("syco whoami printed %q, want a line %q", got, want)
	}
}

func TestCommandErrorsShowControlCharactersAsCarets(t *testing.T) {
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	stderr := os.Stderr
	os.Stderr = w
	root := Root()
	root.SetArgs([]string{"--x\x1b[2J"})
	root.SetOut(io.Discard)
	_ = root.Execute()
	os.Stderr = stderr
	w.Close()
	got, _ := io.ReadAll(r)
	if !strings.Contains(string(got), `x^[[2J`) || strings.ContainsRune(string(got), 0x1b) {
		t.Errorf("syco printed the error %q, want ESC shown as ^[", got)
	}
}

func TestCommandErrorsKeepACharacterSplitAcrossWritesWhole(t *testing.T) {
	var out bytes.Buffer
	w := &caretWriter{Writer: &out}
	for _, s := range []string{"é"[:1], "é"[1:], "😀"[:3], "😀"[3:], "\n"} {
		if _, err := w.Write([]byte(s)); err != nil {
			t.Fatal(err)
		}
	}
	if got, want := out.String(), "é😀\n"; got != want {
		t.Errorf("syco printed %q, want %q", got, want)
	}
}
