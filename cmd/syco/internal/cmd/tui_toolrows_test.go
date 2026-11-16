// The tests in this file cover tool calls in the transcript: one row per
// call, live and from history, each ending with how the call went.

package cmd

import (
	"encoding/json"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
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

// toolRowText is the one row of tool name in view, trailing blanks dropped,
// or "" when there is not exactly one.
func toolRowText(view, name string) string {
	rows := linesWith(view, "⚙ "+name)
	if len(rows) != 1 {
		return ""
	}
	return strings.TrimRight(rows[0], " ")
}

func TestTUILiveToolCallIsOneRowWithItsArgumentThatTurnsFromRunningToCheckedInPlace(t *testing.T) {
	const msgID = "msg-tool-live"
	_, d, s := tuiTurn(t, msgID)
	d.update(tea.WindowSizeMsg{Width: 100, Height: 30})
	s.push(toolStartFrame(msgID, "tool-1", "Read"))
	// The input arrives in pieces, as the model writes it.
	s.push(toolInputFrame(msgID, "tool-1", `{"file_pa`))
	s.push(toolInputFrame(msgID, "tool-1", `th":"/srv/ToolPathOcelot.txt"}`))
	// The model has written the call. It has not run yet.
	s.push(itemStopFrame(msgID, "tool-1", outcomeNone))
	s.push(streamTextFrames(msgID, "item-after", "AfterWrittenStopOcelot")...)

	const running = "⚙ Read  /srv/ToolPathOcelot.txt …"
	d.waitView("the running tool row", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, running) && strings.Contains(v, "AfterWrittenStopOcelot")
	})
	v := d.view()
	if rows := linesWith(v, "⚙ Read"); len(rows) != 1 {
		t.Fatalf("running tool call shows %d rows, want one:\n%s", len(rows), v)
	}
	at := rowIndex(strings.Split(v, "\n"), running)

	s.push(itemStopFrame(msgID, "tool-1", outcomeDone))
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
	raw := d.rawLineWith(done)
	if !strings.Contains(raw, toolStyle.Render("⚙ Read")) ||
		!strings.Contains(raw, statusStyle.Render("/srv/ToolPathOcelot.txt")) {
		t.Fatalf("tool row is not the tool colour with its argument dim:\n%q", raw)
	}
	if !strings.Contains(raw, lipgloss.NewStyle().Foreground(plainGreen).Render(" ✓")) {
		t.Fatalf("✓ is not drawn in plain green:\n%q", raw)
	}
}

// Each outcome the harness reports ends the call's row. A call the user
// stopped keeps its row, ending "stopped", once the turn is cancelled too.
func TestTUILiveToolRowEndsWithTheOutcomeTheHarnessReports(t *testing.T) {
	for _, tc := range []struct {
		name    string
		outcome commonpb.ToolOutcome
		turn    commonpb.TurnState
		tail    string
		style   lipgloss.Style
	}{
		{"failed", outcomeFailed, commonpb.TurnState_TURN_STATE_WORKING, "×", errorStyle},
		{"stopped", outcomeCanceled, commonpb.TurnState_TURN_STATE_CANCELLED, "stopped", statusStyle},
	} {
		t.Run(tc.name, func(t *testing.T) {
			const msgID = "msg-tool-outcome"
			_, d, s := tuiTurn(t, msgID)
			d.update(tea.WindowSizeMsg{Width: 100, Height: 30})
			writeToolCall(s, msgID, "tool-1", "Shell", `{"command":"ls OutcomeArgTern"}`)
			s.push(itemStopFrame(msgID, "tool-1", tc.outcome))
			if tc.turn == commonpb.TurnState_TURN_STATE_CANCELLED {
				s.push(turnStateFrame(msgID, tc.turn))
				d.waitView("the cancelled turn", time.Now().Add(5*time.Second), func(v string) bool {
					return strings.Contains(v, "Turn cancelled")
				})
			}
			want := "   ⚙ Shell  ls OutcomeArgTern " + tc.tail
			d.waitView("the ended tool row", time.Now().Add(5*time.Second), func(v string) bool {
				return toolRowText(v, "Shell") == want
			})
			if raw := d.rawLineWith("⚙ Shell"); !strings.Contains(raw, tc.style.Render(" "+tc.tail)) {
				t.Fatalf("%q is not drawn in its style:\n%q", tc.tail, raw)
			}
		})
	}
}

// A call the model was still writing when the user stopped the turn never
// ran, so its row goes. A call that already ran keeps its row.
func TestTUIToolCallStillBeingWrittenWhenTheTurnIsStoppedLeavesTheLiveChat(t *testing.T) {
	const msgID = "msg-tool-cancel"
	_, d, s := tuiTurn(t, msgID)
	d.update(tea.WindowSizeMsg{Width: 100, Height: 30})
	writeToolCall(s, msgID, "tool-0", "Read", `{"file_path":"/srv/RanBeforeMerlin"}`)
	s.push(itemStopFrame(msgID, "tool-0", outcomeDone))
	s.push(toolStartFrame(msgID, "tool-1", "Shell"))
	s.push(toolInputFrame(msgID, "tool-1", `{"command":"sleep ToolSleepMerlin`))
	d.waitView("the call being written", time.Now().Add(5*time.Second), func(v string) bool {
		return len(linesWith(v, "⚙ Shell")) == 1
	})
	s.push(turnStateFrame(msgID, commonpb.TurnState_TURN_STATE_CANCELLED))
	d.waitView("the cancelled turn", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, "Turn cancelled")
	})
	v := d.view()
	if rows := linesWith(v, "⚙ Shell"); len(rows) != 0 {
		t.Fatalf("the call still being written shows %q, want no row:\n%s", rows, v)
	}
	if got := toolRowText(v, "Read"); got != "   ⚙ Read  /srv/RanBeforeMerlin ✓" {
		t.Fatalf("the call that ran is %q, want its ✓ row kept:\n%s", got, v)
	}
}

func TestTUIToolCallOpenWhenTheTurnFailsEndsWithACross(t *testing.T) {
	const msgID = "msg-tool-fail"
	_, d, s := tuiTurn(t, msgID)
	d.update(tea.WindowSizeMsg{Width: 100, Height: 30})
	writeToolCall(s, msgID, "tool-1", "Shell", `{"command":"ls FailOpenGodwit"}`)
	s.push(failedTurnFrame(msgID, "FailReasonGodwit"))
	d.waitView("the failed turn", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, "FailReasonGodwit")
	})
	if got := toolRowText(d.view(), "Shell"); got != "   ⚙ Shell  ls FailOpenGodwit ×" {
		t.Fatalf("open call of a failed turn is %q, want it ending ×", got)
	}
}

// The turn ends while the call is open: neither the turn end nor the reload
// it brings, whose history has no result for the call, marks it ✓.
func TestTUITurnThatEndsWithACallStillOpenMarksNoCheck(t *testing.T) {
	const msgID = "msg-tool-idle"
	f, d, s := tuiTurn(t, msgID)
	d.update(tea.WindowSizeMsg{Width: 100, Height: 30})
	writeToolCall(s, msgID, "tool-1", "Shell", `{"command":"ls IdleOpenSanderling"}`)
	d.waitView("the open call", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, "IdleOpenSanderling")
	})
	f.appendHistory(toolCallEntry("tool-1", "Shell", `{"command":"ls IdleOpenSanderling"}`))
	d.finishTurn(s, msgID)
	d.waitReload("the turn end's reload")
	v := d.view()
	if got := toolRowText(v, "Shell"); got != "   ⚙ Shell  ls IdleOpenSanderling …" {
		t.Fatalf("call open at the turn end is %q, want it still open:\n%s", got, v)
	}
}

// A reload draws each call from the outcome saved for it. A result saved
// before outcomes were shows by its error flag. A call with no result is
// open.
func TestTUIReloadedToolRowsEndWithTheOutcomeHistorySaved(t *testing.T) {
	f, _ := newClientEnv(t)
	result := func(id string, outcome commonpb.ToolOutcome, isError bool) *commonpb.HistoryEntry {
		e := toolResultEntry(id)
		e.Outcome = outcome
		e.GetMessage().IsError = &isError
		return e
	}
	cases := []struct {
		name   string
		result *commonpb.HistoryEntry
		tail   string
	}{
		{"SavedDoneKnot", result("a", outcomeDone, false), "✓"},
		{"SavedFailedKnot", result("b", outcomeFailed, true), "×"},
		{"SavedStoppedKnot", result("c", outcomeCanceled, true), "stopped"},
		{"OldErrorKnot", result("d", outcomeNone, true), "×"},
		{"OldOkKnot", result("e", outcomeNone, false), "✓"},
		{"NoResultKnot", nil, "…"},
	}
	entries := []*commonpb.HistoryEntry{userEntry("UserReloadOutcomesKnot")}
	for i, c := range cases {
		entries = append(entries, toolCallEntry(string(rune('a'+i)), c.name, "{}"))
		if c.result != nil {
			entries = append(entries, c.result)
		}
	}
	f.setHistory(entries...)
	d := openTUI(t, f)
	f.nextStream(t, 5*time.Second)
	d.waitView("the tool rows", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, "NoResultKnot")
	})
	v := d.view()
	for _, c := range cases {
		if got, want := toolRowText(v, c.name), "   ⚙ "+c.name+" "+c.tail; got != want {
			t.Errorf("%s row is %q, want %q", c.name, got, want)
		}
	}
}

func TestTUIToolArgumentIsOneLineCutToTheScreenWidth(t *testing.T) {
	f, _ := newClientEnv(t)
	cmd := "echo ToolHeadAvocet &&\n  echo ToolSecondLineStilt && " + strings.Repeat("x", 120) + " ToolTailCurlew"
	b, _ := json.Marshal(cmd)
	f.setHistory(userEntry("UserToolLongGrebe"), toolCallEntry("tc-1", "Shell", `{"command":`+string(b)+`}`), toolResultEntry("tc-1"))
	d := openTUI(t, f)
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
	f, _ := newClientEnv(t)
	cases := []struct{ name, input, want string }{
		// A known tool shows its field, not the first string field.
		{"Write", `{"content":"body","file_path":"/r/FieldWriteSkua"}`, "⚙ Write  /r/FieldWriteSkua ✓"},
		// An unknown tool shows its first string field.
		{"FrobnicateWidget", `{"count":3,"target":"FirstStringPuffin","other":"SecondStringTern"}`, "⚙ FrobnicateWidget  FirstStringPuffin ✓"},
		// No string field, or input that does not parse: the name alone.
		{"ReadArchive", `{"archive":1}`, "⚙ ReadArchive ✓"},
		{"BrokenInputTool", `{"path":`, "⚙ BrokenInputTool ✓"},
	}
	entries := []*commonpb.HistoryEntry{userEntry("UserToolFieldsGannet")}
	for i, c := range cases {
		id := string(rune('a' + i))
		entries = append(entries, toolCallEntry(id, c.name, c.input), toolResultEntry(id))
	}
	f.setHistory(entries...)
	d := openTUI(t, f)
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
