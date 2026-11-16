package cmd

import (
	"encoding/json"
	"strings"

	"github.com/charmbracelet/x/ansi"

	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

type toolState int

const (
	toolRunning toolState = iota
	toolDone
	toolFailed
	toolStopped
)

// toolStateOf is the row state outcome gives, and false when outcome says
// nothing of how the call went.
func toolStateOf(outcome commonpb.ToolOutcome) (toolState, bool) {
	switch outcome {
	case commonpb.ToolOutcome_TOOL_OUTCOME_DONE:
		return toolDone, true
	case commonpb.ToolOutcome_TOOL_OUTCOME_FAILED:
		return toolFailed, true
	case commonpb.ToolOutcome_TOOL_OUTCOME_CANCELED:
		return toolStopped, true
	}
	return toolRunning, false
}

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
	head := "⚙ " + caretControls(r.name, "")
	tail, tailStyle := " …", toolStyle
	switch r.state {
	case toolDone:
		tail, tailStyle = " ✓", doneStyle
	case toolFailed:
		tail, tailStyle = " ×", errorStyle
	case toolStopped:
		tail, tailStyle = " stopped", statusStyle
	}
	line := toolStyle.Render(head)
	if arg := toolArg(r.name, r.input.String()); arg != "" {
		if room := width - ansi.StringWidth(head) - 2 - ansi.StringWidth(tail); room >= 2 {
			line += "  " + statusStyle.Render(ansi.Truncate(caretControls(arg, ""), room, "…"))
		}
	}
	return line + tailStyle.Render(tail)
}
