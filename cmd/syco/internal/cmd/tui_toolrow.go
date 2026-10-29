package cmd

import (
	"encoding/json"
	"strings"

	"github.com/charmbracelet/x/ansi"
)

type toolState int

const (
	toolRunning toolState = iota
	toolDone
	// toolEnded: its turn failed or was cancelled before the call
	// stopped; the client cannot tell how the call went.
	toolEnded
)

type toolRow struct {
	name  string
	input strings.Builder
	state toolState
}

// toolArgField names, for each tool the harness and the stock toolsets
// define, the input field that says what the call touched.
var toolArgField = map[string]string{
	"Shell":            "command",
	"Read":             "file_path",
	"Write":            "file_path",
	"Edit":             "file_path",
	"Search":           "pattern",
	"WebFetch":         "url",
	"GetSecret":        "name",
	"ReadInstructions": "path",
	"DispatchAgent":    "path",
	"Think":            "note",
	"RevealPath":       "path",
	"RequestUserInput": "prompt",
	"RequestUserAuth":  "url",
}

func toolArg(name, inputJSON string) string {
	dec := json.NewDecoder(strings.NewReader(inputJSON))
	if tok, err := dec.Token(); err != nil || tok != json.Delim('{') {
		return ""
	}
	want := toolArgField[name]
	var first, found string
	for dec.More() {
		tok, err := dec.Token()
		if err != nil {
			return ""
		}
		key, _ := tok.(string)
		var raw json.RawMessage
		if err := dec.Decode(&raw); err != nil {
			return ""
		}
		var s string
		if json.Unmarshal(raw, &s) != nil {
			continue
		}
		if first == "" {
			first = s
		}
		if key == want && found == "" {
			found = s
		}
	}
	if _, err := dec.Token(); err != nil {
		return ""
	}
	if found == "" {
		found = first
	}
	return oneLine(found)
}

func oneLine(s string) string {
	var parts []string
	for _, l := range strings.Split(s, "\n") {
		if l = strings.TrimSpace(l); l != "" {
			parts = append(parts, l)
		}
	}
	return strings.Join(parts, " ")
}

func (r *toolRow) render(width int) string {
	head := "⚙ " + r.name
	var tail string
	switch r.state {
	case toolRunning:
		tail = " …"
	case toolDone:
		tail = " ✓"
	}
	line := toolStyle.Render(head)
	if arg := toolArg(r.name, r.input.String()); arg != "" {
		if room := width - ansi.StringWidth(head) - 2 - ansi.StringWidth(tail); room >= 2 {
			line += "  " + statusStyle.Render(ansi.Truncate(arg, room, "…"))
		}
	}
	return line + toolStyle.Render(tail)
}
