// The tests in this file cover what the transcript holds besides the conversation.

package cmd

import (
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

func TestTUILineUnderThePanelShowsTheReconnectReasonRedAndCutToTheScreenWidth(t *testing.T) {
	f, d, stream := connectedTUI(t, "conv-notice-drop")
	d.update(tea.WindowSizeMsg{Width: 80, Height: 24})
	d.send("UserNoticeDropPlover")
	d.fastRetries()
	reason := "GatewayDownPetrel " + strings.Repeat("y", 100)
	f.failReceives(1, reason)

	stream.end()
	d.waitView("the wait after the failed reconnect", time.Now().Add(5*time.Second), func(string) bool {
		return strings.Contains(d.footer(), "GatewayDownPetrel")
	})
	left := d.footerLeft()
	if !strings.HasPrefix(left, "disconnected · GatewayDownPetrel y") || !strings.HasSuffix(left, "… · reconnecting in 2s") {
		t.Fatalf("line under the panel is %q, want disconnected, the reason cut with …, then the countdown", left)
	}
	lines := strings.Split(d.view(), "\n")
	if w := ansi.StringWidth(lines[panelEdge(t, lines)+1]); w > 80 {
		t.Fatalf("line under the panel is %d wide, want at most the screen's 80", w)
	}
	red := strings.Split(errorStyle.Render("X"), "X")[0]
	if raw := d.rawLineWith("GatewayDownPetrel"); !strings.Contains(raw, red+"disconnected · GatewayDownPetrel") ||
		!strings.Contains(raw, statusStyle.Render(" · reconnecting in 2s")) {
		t.Fatalf("the reason is not red beside disconnected or the countdown not dim:\n%q", raw)
	}
}

func TestTUITurnFailedShowsInTheTranscriptAsOneRedLine(t *testing.T) {
	const conv = "conv-notice-turns"
	_, d, s := tuiTurn(t, conv)
	d.update(tea.WindowSizeMsg{Width: 120, Height: 30})
	s.push(failedTurnFrame(conv, "TurnBrokeLoon\nTurnBrokeTail"))
	const failLine = "⚠ Turn failed: TurnBrokeLoon TurnBrokeTail"
	d.waitView("the failed turn", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, failLine)
	})
	if raw := d.rawLineWith(failLine); !strings.Contains(raw, errorStyle.Render(failLine)) {
		t.Fatalf("failed turn is not one red line:\n%q", raw)
	}
}
