// The tests in this file cover the chat input.

package cmd

import (
	"fmt"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"

	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

// Some terminals send Shift+Enter as Escape then Enter, which arrives as
// Alt+Enter.
func TestTUIShiftEnterCtrlJAndAltEnterStartNewLinesThatEnterSendsAsOneMessage(t *testing.T) {
	f, d, _ := connectedTUI(t, "conv-multiline")

	d.update(keyText("LineOneHeron"))
	d.update(keyPress(tea.KeyEnter, tea.ModShift))
	d.update(keyText("LineTwoIbis"))
	d.update(keyPress('j', tea.ModCtrl))
	d.update(keyText("LineThreeKite"))
	d.update(keyPress(tea.KeyEnter, tea.ModAlt))
	d.update(keyText("LineFourGecko"))
	const want = "LineOneHeron\nLineTwoIbis\nLineThreeKite\nLineFourGecko"
	d.wantInput("Shift+Enter, Ctrl+J and Alt+Enter", want)

	d.update(keyPress(tea.KeyEnter))
	d.waitIngests(f, 1)
	if got := ingestText(f.ingestLog()[0]); got != want {
		t.Fatalf("ingest carries %q, want the four lines %q", got, want)
	}
	d.waitView("the message on three transcript lines", time.Now().Add(5*time.Second), func(v string) bool {
		lines := strings.Split(v, "\n")
		for i, l := range lines {
			if strings.TrimRight(l, " ") == "┃  LineOneHeron" && i+2 < len(lines) {
				return strings.TrimRight(lines[i+1], " ") == "┃  LineTwoIbis" &&
					strings.TrimRight(lines[i+2], " ") == "┃  LineThreeKite"
			}
		}
		return false
	})
}

func TestTUIPastedLinesKeepTheirBreaksAndOnlyOuterBlankSpaceIsTrimmed(t *testing.T) {
	f, d, _ := connectedTUI(t, "conv-paste")

	d.update(tea.PasteMsg{Content: "\n  \nPasteOneLynx\n\n  PasteTwoMink  \n\n"})
	d.update(keyPress(tea.KeyEnter))
	d.waitIngests(f, 1)
	const want = "PasteOneLynx\n\n  PasteTwoMink"
	if got := ingestText(f.ingestLog()[0]); got != want {
		t.Fatalf("ingest carries %q, want %q", got, want)
	}
}

func TestTUIInputPastSixLinesScrollsInsideASixLinePanelWhileTheScreenStaysFilled(t *testing.T) {
	_, d, _ := connectedTUI(t, "conv-grow")
	d.update(tea.WindowSizeMsg{Width: 80, Height: 30})
	// The first message leaves the start screen for the chat layout.
	d.send("UserGrowFirstGannet")

	var typed []string
	for i := 1; i <= 10; i++ {
		typed = append(typed, fmt.Sprintf("GrowLine%02d", i))
	}
	d.typeLines(typed...)
	if got := strings.Count(d.inputValue(), "\n") + 1; got != 10 {
		t.Fatalf("input holds %d lines, want all 10 typed", got)
	}
	lines := strings.Split(d.view(), "\n")
	if len(lines) != 30 {
		t.Fatalf("view is %d lines with a ten-line input, want the screen's 30:\n%s", len(lines), d.view())
	}
	// Below the input: a blank row, the info row, the bottom edge, the footer.
	top := panelTop(t, lines)
	if top != len(lines)-4-6-1 {
		t.Fatalf("panel starts at %d of %d, want the input capped at six lines in it:\n%s", top, len(lines), d.view())
	}
	if l := lines[top+6]; panelText(l) != "GrowLine10" {
		t.Fatalf("last input line is %q, want the cursor's line GrowLine10 in view", l)
	}
}

func TestTUIQueuedMultiLineMessageShowsOnOneLineAndSendsWithItsBreaks(t *testing.T) {
	const conv = "conv-queue-multiline"
	f, d, stream := tuiTurn(t, conv)

	d.typeLines("QueuedLineOneOtter", "QueuedLineTwoSeal")
	d.update(keyPress(tea.KeyEnter))
	lines := strings.Split(d.view(), "\n")
	s := panelTop(t, lines)
	if want := "queued: QueuedLineOneOtter ⏎ QueuedLineTwoSeal"; !strings.Contains(lines[s-1], want) {
		t.Fatalf("line above the panel is %q, want %q", lines[s-1], want)
	}

	stream.push(turnStateFrame(conv, commonpb.TurnState_TURN_STATE_IDLE))
	d.waitIngests(f, 2)
	if got := ingestText(f.ingestLog()[1]); got != "QueuedLineOneOtter\nQueuedLineTwoSeal" {
		t.Fatalf("queued message sent as %q, want its line break kept", got)
	}
}
