package config

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
)

// InputHistoryFile is shared across agents.
const InputHistoryFile = "input_history"

const inputHistoryLimit = 200

// LoadInputHistory reads the sent messages, oldest first. Each line of the
// file is one message as a JSON string, so line breaks round-trip. A
// missing or unreadable file is an empty history, and damaged lines are
// skipped.
func LoadInputHistory(configDir string) []string {
	data, err := os.ReadFile(filepath.Join(configDir, InputHistoryFile))
	if err != nil {
		return nil
	}
	var entries []string
	for _, line := range bytes.Split(data, []byte("\n")) {
		var msg string
		if json.Unmarshal(line, &msg) == nil {
			entries = append(entries, msg)
		}
	}
	return capInputHistory(entries)
}

func AddInputHistory(entries []string, msg string) []string {
	if n := len(entries); n > 0 && entries[n-1] == msg {
		return entries
	}
	return capInputHistory(append(entries, msg))
}

// AppendInputHistory adds msg to the history on disk and returns the
// history as now saved. It rereads the file first so that TUIs in other
// terminals keep each other's messages.
func AppendInputHistory(configDir, msg string) ([]string, error) {
	entries := AddInputHistory(LoadInputHistory(configDir), msg)
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	for _, e := range entries {
		enc.Encode(e)
	}
	return entries, writeReplacing(configDir, InputHistoryFile, buf.Bytes())
}

func capInputHistory(entries []string) []string {
	if over := len(entries) - inputHistoryLimit; over > 0 {
		entries = entries[over:]
	}
	return entries
}
