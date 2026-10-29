// The tests in this file cover losing the relay: reconnecting with
// backoff and where the wait counts down.

package cmd

import (
	"errors"
	"regexp"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

// startNotice is the start screen's line under the line beneath the
// panel, or "" when there is none (a blank line or the corners row).
func (d *tuiDriver) startNotice() string {
	lines := strings.Split(d.view(), "\n")
	at := panelEdge(d.t, lines) + 2
	if at >= len(lines)-1 {
		return ""
	}
	return strings.TrimSpace(lines[at])
}

func TestTUIStartScreenShowsTheConnectErrorRedAndTheCountdownDimOnOneLineUnderTheLineBelowThePanelUntilConnected(t *testing.T) {
	f, _ := newClientEnv(t)
	f.failReceives(1, "GatewayDownHeron")
	d := openTUI(t, false, "")
	d.fastRetries()
	d.update(tea.WindowSizeMsg{Width: 80, Height: 24})

	d.waitView("the connect error and the countdown", time.Now().Add(5*time.Second), func(string) bool {
		return strings.Contains(d.startNotice(), "GatewayDownHeron")
	})
	if got, want := d.startNotice(), "⚠ GatewayDownHeron · reconnecting in 1s…"; got != want {
		t.Fatalf("line under the line beneath the panel is %q, want %q:\n%s", got, want, d.view())
	}
	raw := d.rawLineWith("GatewayDownHeron")
	if !strings.Contains(raw, errorStyle.Render("⚠ GatewayDownHeron")) ||
		!strings.Contains(raw, statusStyle.Render(" · reconnecting in 1s…")) {
		t.Fatalf("error is not red or the countdown is not dim:\n%q", raw)
	}

	// Narrow: cut to the width on one line, the screen still exactly full.
	d.update(tea.WindowSizeMsg{Width: 30, Height: 24})
	lines := strings.Split(d.view(), "\n")
	if len(lines) != 24 {
		t.Fatalf("narrow view is %d lines, want 24:\n%s", len(lines), d.view())
	}
	n := d.startNotice()
	if !strings.HasPrefix(n, "⚠ Gateway") || !strings.HasSuffix(n, "…") || strings.Contains(n, "reconnecting") {
		t.Fatalf("narrow notice is %q, want it cut to the width with an ellipsis", n)
	}
	for i, l := range lines {
		if w := ansi.StringWidth(l); w > 30 {
			t.Fatalf("narrow line %d is %d wide: %q", i, w, l)
		}
	}
	d.update(tea.WindowSizeMsg{Width: 80, Height: 24})

	d.waitView("connected with the notice gone", time.Now().Add(5*time.Second), func(string) bool {
		return strings.HasPrefix(d.footer(), "tester · ")
	})
	if n := d.startNotice(); n != "" {
		t.Fatalf("notice %q still shows after connecting:\n%s", n, d.view())
	}
}

var countdownRe = regexp.MustCompile(`reconnecting in (\d+)s`)

func TestTUIReconnectWaitsDoubleFromOneSecondToAThirtySecondCapAndCountDownEachSecond(t *testing.T) {
	f, _ := newClientEnv(t)
	f.failReceives(1000, "GatewayDownPetrel")
	d := openTUI(t, false, "")
	d.fastRetries()
	d.update(tea.WindowSizeMsg{Width: 80, Height: 24})

	waits := []int{1, 2, 4, 8, 16, 30, 30}
	var want []int
	for _, w := range waits {
		for s := w; s >= 1; s-- {
			want = append(want, s)
		}
	}

	// Every countdown value shown, in order, once per stretch it shows.
	var seen []int
	last := -1
	deadline := time.After(10 * time.Second)
	for len(seen) < len(want) {
		if m := countdownRe.FindStringSubmatch(d.startNotice()); m != nil {
			s, _ := strconv.Atoi(m[1])
			if s != last {
				seen = append(seen, s)
			}
			last = s
		} else {
			last = -1
		}
		select {
		case msg := <-d.msgs:
			d.update(msg)
		case <-deadline:
			t.Fatalf("timed out; countdown so far %v, want %v", seen, want)
		}
	}
	if !slices.Equal(seen, want) {
		t.Fatalf("countdown went %v, want %v (waits of %v seconds)", seen, want, waits)
	}
}

func TestTUIReconnectWaitStartsAgainAtOneSecondAfterAReconnect(t *testing.T) {
	f, d, stream := connectedTUI(t, "conv-reconnect-chat")
	d.update(tea.WindowSizeMsg{Width: 80, Height: 24})
	d.send("UserBeforeDropStork")
	d.fastRetries()

	stream.end()
	d.waitView("the first wait", time.Now().Add(5*time.Second), func(string) bool {
		return d.footerLeft() == "disconnected · reconnecting in 1s"
	})

	d.waitView("connected again", time.Now().Add(5*time.Second), func(string) bool {
		return strings.HasPrefix(d.footerLeft(), "tester · ")
	})
	again := f.nextStream(t, time.Second)
	again.end()
	d.waitView("the wait after the next drop", time.Now().Add(5*time.Second), func(string) bool {
		return strings.HasPrefix(d.footerLeft(), "disconnected · reconnecting")
	})
	if got := d.footerLeft(); got != "disconnected · reconnecting in 1s" {
		t.Fatalf("after a reconnect the next wait shows %q, want it back at 1s", got)
	}
}

func TestTUIRetriedFirstConnectAfterThePickerOpensTheConversationContinueNames(t *testing.T) {
	f, dir := newClientEnv(t)
	const conv = "conv-picker-retry"
	f.setHistory(conv, userEntry("UserEarlierGannet"), assistantEntry("AssistantEarlierShag"))
	saveHistoryLine(t, dir, conv)
	f.failReceives(1, "GatewayDownSkua")

	cfgDir, p, token := testProfile(t)
	p.Workspace = ""
	d := newTUIDriver(t, newTUIModel(cfgDir, p, token, true, "", nil))
	d.fastRetries()
	d.update(workspacesMsg{workspaces: []string{testWorkspace}})
	d.update(tea.WindowSizeMsg{Width: 160, Height: 40})
	d.waitView("the history --continue names after the retried connect", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, "UserEarlierGannet") && strings.Contains(v, "AssistantEarlierShag")
	})
	if n := f.attempts(); n != 2 {
		t.Fatalf("fake saw %d connect attempts, want the failed one and the retry", n)
	}
}

func TestTUIStreamErrorsDuringARetryOrFromAReplacedStreamStartNoSecondReconnect(t *testing.T) {
	f, d, stream := connectedTUI(t, "conv-reconnect-stale")
	d.update(tea.WindowSizeMsg{Width: 80, Height: 24})
	d.send("UserBeforeDropTeal")
	d.model.(*tuiModel).retryTick = time.Hour
	old := d.model.(*tuiModel).stream

	stream.end()
	d.waitView("the first wait", time.Now().Add(5*time.Second), func(string) bool {
		return d.footerLeft() == "disconnected · reconnecting in 1s"
	})
	// An ingest failing while the wait runs is the same loss.
	d.update(streamErrMsg{err: errors.New("IngestFailedGrebe")})
	if got := d.model.(*tuiModel).backoff; got != 2*time.Second {
		t.Fatalf("after a second error in the wait the next wait is %v, want 2s (one retry, not two)", got)
	}

	d.update(retryTickMsg{})
	d.waitView("connected again", time.Now().Add(5*time.Second), func(string) bool {
		return strings.HasPrefix(d.footerLeft(), "tester · ")
	})
	f.nextStream(t, time.Second)
	// The replaced stream's error arrives late.
	d.update(streamErrMsg{err: errors.New("OldStreamClosedPipit"), stream: old})
	if left := d.footerLeft(); !strings.HasPrefix(left, "tester · ") {
		t.Fatalf("the replaced stream's error disconnected the new one: %q", left)
	}
}

func TestTUICtrlCDuringAReconnectWaitQuitsAndStopsRetrying(t *testing.T) {
	f, _ := newClientEnv(t)
	f.failReceives(1000, "GatewayDownAuk")
	d := openTUI(t, false, "")
	d.model.(*tuiModel).retryTick = 20 * time.Millisecond
	d.update(tea.WindowSizeMsg{Width: 80, Height: 24})
	d.waitView("a reconnect wait", time.Now().Add(5*time.Second), func(string) bool {
		return strings.Contains(d.startNotice(), "reconnecting in")
	})
	tried := f.attempts()

	d.update(keyPress('c', tea.ModCtrl))
	d.waitMsg("the quit", time.Second, func(msg tea.Msg) bool {
		_, ok := msg.(tea.QuitMsg)
		return ok
	})
	d.settle(300 * time.Millisecond)
	if n := f.attempts(); n != tried {
		t.Fatalf("fake saw %d more connect attempts after Ctrl+C, want none", n-tried)
	}
}
