package config

import (
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

func TestInputHistoryRoundTripsMessagesWithLineBreaks(t *testing.T) {
	dir := t.TempDir()
	want := []string{"FirstHeron\nsecond line\n\n  indented", "tab\there \"quoted\" \\ back"}
	for _, msg := range want {
		if _, err := AppendInputHistory(dir, msg); err != nil {
			t.Fatalf("AppendInputHistory: %v", err)
		}
	}
	if got := LoadInputHistory(dir); !reflect.DeepEqual(got, want) {
		t.Fatalf("LoadInputHistory = %q, want %q", got, want)
	}
}

func TestInputHistorySkipsConsecutiveDuplicates(t *testing.T) {
	dir := t.TempDir()
	var got []string
	for _, msg := range []string{"Otter", "Otter", "Seal", "Otter"} {
		var err error
		if got, err = AppendInputHistory(dir, msg); err != nil {
			t.Fatal(err)
		}
	}
	want := []string{"Otter", "Seal", "Otter"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("AppendInputHistory returned %q, want %q", got, want)
	}
	if got := LoadInputHistory(dir); !reflect.DeepEqual(got, want) {
		t.Fatalf("LoadInputHistory = %q, want %q", got, want)
	}
}

func TestInputHistoryKeepsTheNewest200(t *testing.T) {
	dir := t.TempDir()
	for i := range 205 {
		if _, err := AppendInputHistory(dir, fmt.Sprintf("msg-%03d", i)); err != nil {
			t.Fatal(err)
		}
	}
	got := LoadInputHistory(dir)
	if len(got) != inputHistoryLimit || inputHistoryLimit != 200 {
		t.Fatalf("history holds %d messages (limit %d), want 200", len(got), inputHistoryLimit)
	}
	if got[0] != "msg-005" || got[199] != "msg-204" {
		t.Fatalf("history runs %q..%q, want msg-005..msg-204", got[0], got[199])
	}
}

func TestInputHistoryLoadOfAnUnreadableFileIsEmpty(t *testing.T) {
	dir := t.TempDir()
	if err := os.Mkdir(filepath.Join(dir, InputHistoryFile), 0o755); err != nil {
		t.Fatal(err)
	}
	if got := LoadInputHistory(dir); len(got) != 0 {
		t.Fatalf("unreadable file loaded %q, want empty", got)
	}
}
