// The tests in this file cover tool calls in the transcript: one row per
// call, live and from history.

package cmd

import (
	"encoding/json"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

func linesWith(view, s string) []string {
	var out []string
	for _, l := range strings.Split(view, "\n") {
		if strings.Contains(l, s) {
			out = append(out, l)
		}
	}
	return out
}

func TestTUILiveToolCallIsOneRowWithItsArgumentThatTurnsFromRunningToCheckedInPlace(t *testing.T) {
	const conv = "conv-tool-live"
	_, d, s := tuiTurn(t, conv)
	d.update(tea.WindowSizeMsg{Width: 100, Height: 30})
	s.push(toolStartFrame(conv, "tool-1", "Read"))
	// The input arrives in pieces, as the model writes it.
	s.push(toolInputFrame(conv, "tool-1", `{"file_pa`))
	s.push(toolInputFrame(conv, "tool-1", `th":"/srv/ToolPathOcelot.txt"}`))

	const running = "⚙ Read  /srv/ToolPathOcelot.txt …"
	d.waitView("the running tool row", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, running)
	})
	v := d.view()
	if rows := linesWith(v, "⚙ Read"); len(rows) != 1 {
		t.Fatalf("running tool call shows %d rows, want one:\n%s", len(rows), v)
	}
	at := rowIndex(strings.Split(v, "\n"), running)

	s.push(itemStopFrame(conv, "tool-1"))
	const done = "⚙ Read  /srv/ToolPathOcelot.txt ✓"
	d.waitView("the checked tool row", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, done)
	})
	v = d.view()
	if rows := linesWith(v, "⚙ Read"); len(rows) != 1 {
		t.Fatalf("stopped tool call shows %d rows, want the one row it had:\n%s", len(rows), v)
	}
	if got := rowIndex(strings.Split(v, "\n"), done); got != at {
		t.Fatalf("checked row is on line %d, want line %d where it ran:\n%s", got, at, v)
	}
	if raw := d.rawLineWith(done); !strings.Contains(raw, toolStyle.Render("⚙ Read")) ||
		!strings.Contains(raw, statusStyle.Render("/srv/ToolPathOcelot.txt")) {
		t.Fatalf("tool row is not the tool colour with its argument dim:\n%q", raw)
	}
}

// A cancelled or failed turn says nothing about how its running call went:
// the row loses its running marker and gets no ✓.
func TestTUIRunningToolRowLosesItsMarkerWhenTheTurnIsCancelled(t *testing.T) {
	const conv = "conv-tool-cancel"
	_, d, s := tuiTurn(t, conv)
	d.update(tea.WindowSizeMsg{Width: 100, Height: 30})
	s.push(toolStartFrame(conv, "tool-1", "Shell"))
	s.push(toolInputFrame(conv, "tool-1", `{"command":"sleep ToolSleepMerlin"}`))
	s.push(turnStateFrame(conv, commonpb.TurnState_TURN_STATE_CANCELLED))
	d.waitView("the cancelled turn", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, "Turn cancelled")
	})
	rows := linesWith(d.view(), "⚙ Shell")
	if len(rows) != 1 || strings.TrimRight(rows[0], " ") != "   ⚙ Shell  sleep ToolSleepMerlin" {
		t.Fatalf("cancelled turn's tool row is %q, want it without a marker", rows)
	}
}

func TestTUIToolArgumentIsOneLineCutToTheScreenWidth(t *testing.T) {
	f, dir := newClientEnv(t)
	const conv = "conv-tool-long"
	cmd := "echo ToolHeadAvocet &&\n  echo ToolSecondLineStilt && " + strings.Repeat("x", 120) + " ToolTailCurlew"
	b, _ := json.Marshal(cmd)
	f.setHistory(conv, userEntry("UserToolLongGrebe"), toolCallEntry("tc-1", "Shell", `{"command":`+string(b)+`}`))
	saveHistoryLine(t, dir, conv)
	d := openTUI(t, true, "")
	f.nextStream(t, 5*time.Second)
	d.update(tea.WindowSizeMsg{Width: 64, Height: 20})
	d.waitView("the tool row", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, "⚙ Shell")
	})
	v := d.view()
	rows := linesWith(v, "⚙ Shell")
	if len(rows) != 1 {
		t.Fatalf("long tool call shows %d rows, want one:\n%s", len(rows), v)
	}
	row := strings.TrimRight(rows[0], " ")
	if !strings.Contains(row, "echo ToolHeadAvocet && echo ToolSecondLineStilt") {
		t.Fatalf("row %q does not hold the command's lines joined on one line", row)
	}
	if strings.Contains(v, "ToolTailCurlew") || !strings.HasSuffix(row, "… ✓") {
		t.Fatalf("row %q is not cut with … before its ✓", row)
	}
	if w := ansi.StringWidth(row); w > 64 {
		t.Fatalf("row is %d wide, want at most the screen's 64: %q", w, row)
	}
}

func TestTUIHistoryToolRowsShowTheFieldEachToolTouched(t *testing.T) {
	f, dir := newClientEnv(t)
	const conv = "conv-tool-fields"
	cases := []struct{ name, input, want string }{
		// A known tool shows its field, not the first string field.
		{"Write", `{"content":"body","file_path":"/r/FieldWriteSkua"}`, "⚙ Write  /r/FieldWriteSkua ✓"},
		// An unknown tool shows its first string field.
		{"FrobnicateWidget", `{"count":3,"target":"FirstStringPuffin","other":"SecondStringTern"}`, "⚙ FrobnicateWidget  FirstStringPuffin ✓"},
		// No string field, or input that does not parse: the name alone.
		{"RecentTurns", `{"limit":4}`, "⚙ RecentTurns ✓"},
		{"BrokenInputTool", `{"path":`, "⚙ BrokenInputTool ✓"},
	}
	entries := []*commonpb.HistoryEntry{userEntry("UserToolFieldsGannet")}
	for i, c := range cases {
		entries = append(entries, toolCallEntry(string(rune('a'+i)), c.name, c.input))
	}
	f.setHistory(conv, entries...)
	saveHistoryLine(t, dir, conv)
	d := openTUI(t, true, "")
	f.nextStream(t, 5*time.Second)
	d.waitView("the tool rows", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, "BrokenInputTool")
	})
	v := d.view()
	for _, c := range cases {
		rows := linesWith(v, "⚙ "+c.name+" ")
		if len(rows) != 1 || strings.TrimRight(rows[0], " ") != "   "+c.want {
			t.Errorf("%s row is %q, want %q in column 3", c.name, rows, c.want)
		}
	}
}
