// The tests in this file cover how the transcript draws its blocks, live
// and from history.

package cmd

import (
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

func TestTUISentMessageIsAPanelWithNoLabel(t *testing.T) {
	_, d, s := connectedTUI(t)
	d.update(tea.WindowSizeMsg{Width: 80, Height: 30})
	d.send("UserPanelHeron")
	d.startTurn(s, "msg-rows-panel", "UserPanelHeron")

	lines := d.transcriptLines()
	at := rowIndex(lines, "UserPanelHeron")
	if at < 1 || at+1 >= len(lines) {
		t.Fatalf("message not in the transcript:\n%s", strings.Join(lines, "\n"))
	}
	if got := lines[at]; got != "┃  UserPanelHeron"+strings.Repeat(" ", 80-3-len("UserPanelHeron")) {
		t.Fatalf("message row is %q, want the bar, two blanks and the text, full width with no label", got)
	}
	for _, i := range []int{at - 1, at + 1} {
		if !isPanelRow(lines[i]) || panelRowText(lines[i]) != "" || ansi.StringWidth(lines[i]) != 80 {
			t.Fatalf("row %q next to the message is not a blank padded panel row", lines[i])
		}
	}
	if isPanelRow(lines[at-2]) || isPanelRow(lines[at+2]) {
		t.Fatalf("message panel has more than one padded row above or below:\n%s", strings.Join(lines, "\n"))
	}
	raw := d.rawLineWith("UserPanelHeron")
	if !strings.Contains(raw, panelBar(panelAccent)) || !strings.Contains(raw, panelFill.Render(strings.Repeat(" ", panelPad))) {
		t.Fatalf("message row lacks the accent bar or the panel background:\n%q", raw)
	}
}

func TestTUISentMessageWrapsInsideItsPanelAtANarrowWidth(t *testing.T) {
	_, d, s := connectedTUI(t)
	d.update(tea.WindowSizeMsg{Width: 30, Height: 30})
	words := []string{"WrapOneAlbatross", "WrapTwoBittern", "WrapThreeCurlew", "WrapFourDunlin"}
	d.send(strings.Join(words, " "))
	d.startTurn(s, "msg-rows-wrap", words[0])

	lines := d.transcriptLines()
	first := rowIndex(lines, words[0])
	if first < 0 {
		t.Fatalf("message not in the transcript:\n%s", strings.Join(lines, "\n"))
	}
	var got []string
	for i := first; i < len(lines) && panelRowText(lines[i]) != ""; i++ {
		if !isPanelRow(lines[i]) || ansi.StringWidth(lines[i]) != 30 {
			t.Fatalf("wrapped row %q is not a full-width panel row", lines[i])
		}
		if !strings.HasPrefix(lines[i], "┃  ") {
			t.Fatalf("wrapped row %q leaves the panel's padding", lines[i])
		}
		got = append(got, strings.Fields(panelRowText(lines[i]))...)
	}
	if strings.Join(got, " ") != strings.Join(words, " ") {
		t.Fatalf("panel rows hold %q, want the message %q wrapped inside it:\n%s", got, words, strings.Join(lines, "\n"))
	}
}

func TestTUIReplyHasNoLabelAndStartsInColumnThreeLiveAndCommitted(t *testing.T) {
	const msgID = "msg-rows-reply"
	_, d, s := tuiTurn(t, msgID)
	d.update(tea.WindowSizeMsg{Width: 80, Height: 30})
	s.push(streamTextFrames(msgID, "item-1", "ReplyColumnEgret is **bold**\n\n- ListItemKestrel")...)

	check := func(when string) {
		t.Helper()
		lines := d.transcriptLines()
		at := rowIndex(lines, "ReplyColumnEgret")
		if at < 0 {
			t.Fatalf("%s: reply not in the transcript:\n%s", when, strings.Join(lines, "\n"))
		}
		if col := textColumn(lines[at]); col != 3 {
			t.Fatalf("%s: reply text starts in column %d, want 3: %q", when, col, lines[at])
		}
	}
	d.waitView("the live reply", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, "ReplyColumnEgret")
	})
	check("while streaming")

	d.finishTurn(s, msgID)
	check("committed")
	lines := d.transcriptLines()
	if at := rowIndex(lines, "ListItemKestrel"); at < 0 || textColumn(lines[at]) != 3 {
		t.Fatalf("list item is not in column 3:\n%s", strings.Join(lines, "\n"))
	}
}

// conversationBlocks returns the TUI after a second message's turn starts,
// and the transcript as the failed first turn left it, before that reload.
func conversationBlocks(t *testing.T) (*tuiDriver, []string) {
	t.Helper()
	const msgID = "msg-rows-blocks"
	f, d, s := tuiTurn(t, msgID)
	d.update(tea.WindowSizeMsg{Width: 80, Height: 40})
	s.push(streamTextFrames(msgID, "item-1", "ReplyBlocksOriole\n\n- BlocksListPlover")...)
	s.push(toolStartFrame(msgID, "tool-1", "Read"))
	s.push(toolStartFrame(msgID, "tool-2", "Search"))
	s.push(failedTurnFrame(msgID, "BlocksFailQuail"))
	d.waitView("the failed turn", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, "BlocksFailQuail")
	})
	live := d.transcriptLines()
	f.appendHistory(assistantEntry("ReplyBlocksOriole\n\n- BlocksListPlover"),
		toolCallEntry("tool-1", "Read", "{}"), toolCallEntry("tool-2", "Search", "{}"))
	d.send("UserBlocksRedstart")
	d.startTurn(s, "msg-rows-blocks-2", "UserBlocksRedstart")
	return d, live
}

func TestTUIOnlyABarOrBlanksLeftOfColumnThree(t *testing.T) {
	d, live := conversationBlocks(t)
	lines := d.transcriptLines()
	for _, ls := range [][]string{live, lines} {
		for _, l := range ls {
			r := []rune(l + "   ")
			for c := 0; c < 3; c++ {
				if r[c] != ' ' && !(c == 0 && r[c] == '┃') {
					t.Fatalf("line %q has %q in column %d, want only a bar or a blank left of column 3", l, r[c], c)
				}
			}
		}
	}
	for _, s := range []string{"⚙ Read", "⚠ Turn failed: BlocksFailQuail"} {
		if at := rowIndex(lines, s); at < 0 || textColumn(lines[at]) != 3 {
			t.Fatalf("%q does not start in column 3:\n%s", s, strings.Join(lines, "\n"))
		}
	}
}

func TestTUIExactlyOneBlankLineBetweenBlocks(t *testing.T) {
	d, live := conversationBlocks(t)
	lines := d.transcriptLines()
	all := strings.Join(lines, "\n")
	for _, ls := range [][]string{live, lines} {
		start := 0
		for start < len(ls) && isBlank(ls[start]) {
			start++
		}
		for i := start + 1; i < len(ls); i++ {
			if isBlank(ls[i]) && isBlank(ls[i-1]) {
				t.Fatalf("two blank lines in a row at line %d:\n%s", i, strings.Join(ls, "\n"))
			}
		}
	}
	if n := len(lines); !isBlank(lines[n-1]) || isBlank(lines[n-2]) {
		t.Fatalf("want exactly one blank line above the input panel:\n%s", all)
	}
	// Each block's first line has one blank line above it, and the block
	// before ends just above that.
	reply := rowIndex(lines, "ReplyBlocksOriole")
	read := rowIndex(lines, "⚙ Read")
	search := rowIndex(lines, "⚙ Search")
	fail := rowIndex(lines, "BlocksFailQuail")
	second := rowIndex(lines, "UserBlocksRedstart") - 1 // its top padded row
	for name, at := range map[string]int{"reply": reply, "tool rows": read, "notice": fail, "second message": second} {
		if at < 2 || !isBlank(lines[at-1]) || isBlank(lines[at-2]) {
			t.Fatalf("%s does not follow the block before it after exactly one blank line:\n%s", name, all)
		}
	}
	if !isPanelRow(lines[reply-2]) {
		t.Fatalf("reply does not follow the message panel:\n%s", all)
	}
	if search != read+1 {
		t.Fatalf("tool rows are not one group of adjacent lines:\n%s", all)
	}
}

func TestTUIHistoryDrawsTheTranscriptExactlyAsLive(t *testing.T) {
	const msgID = "msg-rows-history"
	f, d, s := connectedTUI(t)
	const reply = "Here is **HistoryReplyRook**:\n\n- HistoryItemSwift\n- HistoryItemTern"
	d.update(tea.WindowSizeMsg{Width: 80, Height: 30})
	d.send("UserHistoryPetrel")
	d.startTurn(s, msgID, "UserHistoryPetrel")
	s.push(streamTextFrames(msgID, "item-1", "HistoryFirstReplyWigeon")...)
	d.finishTurn(s, msgID)
	// The harness writes the reply to the conversation, as the fake's ingest
	// writes each message; the next turn start reloads it.
	f.appendHistory(assistantEntry("HistoryFirstReplyWigeon"))

	d.typeLines("UserHistoryLineOne", "UserHistoryLineTwo")
	d.enter()
	d.startTurn(s, msgID, "UserHistoryLineOne")
	s.push(streamTextFrames(msgID, "item-2", reply)...)
	s.push(toolStartFrame(msgID, "tool-1", "Read"))
	s.push(toolInputFrame(msgID, "tool-1", `{"file_path":"/srv/HistoryToolOne"}`))
	s.push(toolStartFrame(msgID, "tool-2", "Shell"))
	s.push(toolInputFrame(msgID, "tool-2", `{"command":"ls HistoryToolTwo"}`))
	d.finishTurn(s, msgID)
	live := strings.Join(d.transcriptLines(), "\n")

	f.setHistory(
		userEntry("UserHistoryPetrel"),
		assistantEntry("HistoryFirstReplyWigeon"),
		userEntry("UserHistoryLineOne\nUserHistoryLineTwo"),
		assistantEntry(reply),
		toolCallEntry("tool-1", "Read", `{"file_path":"/srv/HistoryToolOne"}`),
		toolCallEntry("tool-2", "Shell", `{"command":"ls HistoryToolTwo"}`))
	// Let the turn end's reload land first: the reconnect closes the
	// connection it reads on.
	d.waitMsg("the turn end's reload", 5*time.Second, func(msg tea.Msg) bool {
		h, ok := msg.(historyMsg)
		return ok && h.gen == d.model.(*tuiModel).reloadGen
	})
	d.dropAndReconnect(s)
	if got := strings.Join(d.transcriptLines(), "\n"); got != live {
		t.Fatalf("history draws:\n%s\nwant the live transcript:\n%s", got, live)
	}
}

func TestTUIResizeRefitsReplies(t *testing.T) {
	const msgID = "msg-rows-resize"
	_, d, s := connectedTUI(t)
	d.update(tea.WindowSizeMsg{Width: 100, Height: 40})
	d.send("ResizeMsgOne ResizeMsgTwo ResizeMsgThree ResizeMsgFour")
	d.startTurn(s, msgID, "ResizeMsgOne")
	s.push(streamTextFrames(msgID, "item-1", "ResizeReplyOne ResizeReplyTwo ResizeReplyThree ResizeReplyFour")...)
	d.finishTurn(s, msgID)

	d.update(tea.WindowSizeMsg{Width: 40, Height: 40})
	lines := d.transcriptLines()
	all := strings.Join(lines, "\n")
	for _, l := range lines {
		if ansi.StringWidth(strings.TrimRight(l, " ")) > 40 {
			t.Fatalf("line %q is wider than the new width 40:\n%s", l, all)
		}
	}
	a, b := rowIndex(lines, "ResizeReplyOne"), rowIndex(lines, "ResizeReplyFour")
	if a < 0 || b <= a || textColumn(lines[b]) != 3 {
		t.Fatalf("reply did not re-wrap to the new width in column 3:\n%s", all)
	}
}
