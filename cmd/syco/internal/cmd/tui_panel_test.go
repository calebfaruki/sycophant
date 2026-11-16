// The tests in this file cover the input panel and the line under it.

package cmd

import (
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/sycophant-md/sycophant/cmd/syco/internal/config"
)

func TestTUILineUnderThePanelShowsWorkingAndEscTakeBackOrStopWhileATurnRunsAndIdentityServerAndNewlineWhenIdle(t *testing.T) {
	const msgID = "msg-footer"
	f, d, stream := tuiTurn(t, msgID)
	if left, hints := d.footerParts(); left != working || hints != "esc take back · stop" {
		t.Fatalf("line under the panel while the turn runs is %q / %q, want %q / %q:\n%s", left, hints, working, "esc take back · stop", d.view())
	}

	d.finishTurn(stream, msgID)
	if left, hints := d.footerParts(); left != "tester · "+f.addr || hints != "shift+enter newline" {
		t.Fatalf("idle line under the panel is %q / %q, want %q / %q", left, hints, "tester · "+f.addr, "shift+enter newline")
	}

	d.update(tea.WindowSizeMsg{Width: 30, Height: 20})
	if left, hints := d.footerParts(); hints != "" || !strings.HasPrefix(left, "tester") {
		t.Fatalf("on a narrow screen the line under the panel is %q / %q, want the status alone", left, hints)
	}
	for _, l := range strings.Split(d.view(), "\n") {
		if w := ansi.StringWidth(l); w > 30 {
			t.Fatalf("line %q is %d wide on a 30-column screen", l, w)
		}
	}
}

func TestTUIPanelBarIsTheAccentYellowWhileAnApprovalWaitsAndRedWhileDisconnected(t *testing.T) {
	_, d, stream := tuiTurn(t, "msg-bar-colour")
	if raw := d.model.View().Content; !barStarts(raw, brandCyan) {
		t.Fatalf("panel bar is not drawn in the accent colour:\n%q", raw)
	}
	stream.push(approvalRequest(t, "req-bar"))
	d.waitMsg("the approval request frame", 5*time.Second, isServerRequestFrame)
	if raw := d.model.View().Content; !barStarts(raw, panelApproval) {
		t.Fatalf("panel bar is not yellow while an approval waits:\n%q", raw)
	}

	off := newTUIModel(t.TempDir(), &config.Profile{Server: "bar.invalid", Agent: "a-bar"}, "")
	off.Update(tea.WindowSizeMsg{Width: 80, Height: 20})
	if raw := off.View().Content; !barStarts(raw, panelDisconnected) {
		t.Fatalf("panel bar is not red while disconnected:\n%q", raw)
	}
}

func TestTUIApprovalShowsInsideThePanelInPlaceOfTheInput(t *testing.T) {
	_, d, stream := tuiTurn(t, "msg-approval-panel")
	stream.push(approvalRequest(t, "req-panel"))
	d.waitMsg("the approval request frame", 5*time.Second, isServerRequestFrame)

	lines := strings.Split(d.view(), "\n")
	top := panelTop(t, lines)
	body := panelText(lines[top+1])
	if !strings.HasPrefix(body, "⚙ "+approvalTool) || strings.Contains(d.view(), placeholder) {
		t.Fatalf("panel's first body row is %q, want the approval prompt in place of the input:\n%s", body, d.view())
	}
}

func TestTUIChatLayoutPanelSitsAboveTheFooterFullWidthWithTheInputAndInfoRow(t *testing.T) {
	const height = 12
	_, d, s := connectedTUI(t)
	d.update(tea.WindowSizeMsg{Width: 80, Height: height})
	// The first message leaves the start screen for the chat layout; it
	// shows at its turn start.
	d.send("UserFillFirstAuk")
	d.startTurn(s, "msg-fill", "UserFillFirstAuk")
	d.finishTurn(s, "msg-fill")
	d.typeLines("FillLineGull00")

	v := d.view()
	lines := strings.Split(v, "\n")
	if len(lines) != height {
		t.Fatalf("view is %d lines, want the screen's %d:\n%s", len(lines), height, v)
	}
	top := panelTop(t, lines)
	// top padding, the input row, a blank row, the info line.
	if want := height - 2 - 4; top != want {
		t.Fatalf("panel starts at line %d, want %d:\n%s", top, want, v)
	}
	if got := panelText(lines[top+1]); got != "FillLineGull00" {
		t.Fatalf("input row is %q, want %q:\n%s", got, "FillLineGull00", v)
	}
	if got := panelText(lines[top+3]); got != testAgent {
		t.Fatalf("info row is %q, want the agent's name %q alone", got, testAgent)
	}
	for i, l := range lines {
		if w := ansi.StringWidth(l); i >= top && i < height-1 && w != 80 {
			t.Fatalf("panel row %d is %d wide, want the screen's 80: %q", i, w, l)
		}
	}
	if !strings.Contains(strings.Join(lines[:top], "\n"), "UserFillFirstAuk") {
		t.Fatalf("the transcript lost its last line to the panel:\n%s", v)
	}
}

func TestTUIDrawsNoBrightGreenOnThePickerTheStartScreenOrTheConversation(t *testing.T) {
	check := func(where, raw string) {
		t.Helper()
		if brightGreenRe.MatchString(raw) {
			t.Fatalf("%s draws bright green:\n%q", where, raw)
		}
	}

	_, d, s := connectedTUI(t)
	d.update(tea.WindowSizeMsg{Width: 80, Height: 24})
	raw := d.model.View().Content
	check("the start screen", raw)
	if !strings.Contains(raw, dot(plainGreen)) {
		t.Fatalf("the connected dot is not drawn in plain green:\n%q", raw)
	}

	const msgID = "msg-colours"
	d.send("UserColoursKite")
	d.startTurn(s, msgID, "UserColoursKite")
	writeToolCall(s, msgID, "tool-1", "Read", `{"file_path":"/srv/ColoursKite"}`)
	s.push(itemStopFrame(msgID, "tool-1", outcomeDone))
	s.push(streamTextFrames(msgID, "item-1", "ReplyColoursKite")...)
	d.waitView("the reply", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, "ReplyColoursKite") && strings.Contains(v, "✓")
	})
	check("the conversation", d.model.View().Content)

	cfgDir, p, token := testProfile(t)
	picker := newTUIModel(cfgDir, p, token)
	picker.phase, picker.pickerLoading = phasePicker, false
	picker.agents = []string{"PickerAgentKite", "PickerOtherKite"}
	check("the agent picker", picker.View().Content)
}

func TestTUISmallWindowDropsTheKeyHintsThenCutsNamesAndAlwaysKeepsTheInputAndMessages(t *testing.T) {
	f, _ := newClientEnv(t)
	const speaker = "PersonWithALongNamePlover"
	f.setHistory(sentBy(personSender(speaker), "Hi"))
	d := openTUI(t, f)
	f.nextStream(t, 5*time.Second)
	d.waitConnected()
	d.waitView("the message", time.Now().Add(5*time.Second), func(string) bool {
		return d.hasSentPanel("Hi")
	})
	identity := "tester · " + f.addr
	const hints = "shift+enter newline"
	fits := ansi.StringWidth(identity) + 2 + len(hints) + 2*panelPad

	d.update(tea.WindowSizeMsg{Width: fits, Height: 14})
	if left, got := d.footerParts(); left != identity || got != hints {
		t.Fatalf("at width %d the footer is %q / %q, want %q / %q", fits, left, got, identity, hints)
	}
	d.update(tea.WindowSizeMsg{Width: fits - 1, Height: 14})
	if left, got := d.footerParts(); left != identity || got != "" {
		t.Fatalf("at width %d the footer is %q / %q, want %q with no key hints", fits-1, left, got, identity)
	}

	const narrow = 9
	d.update(tea.WindowSizeMsg{Width: narrow, Height: 14})
	if got, want := d.footerLeft(), ansi.Truncate(identity, narrow-2*panelPad, "…"); got != want {
		t.Fatalf("at width %d the footer is %q, want the identity cut to %q", narrow, got, want)
	}
	lines := strings.Split(d.view(), "\n")
	if got, want := panelText(lines[panelTop(t, lines)+3]), ansi.Truncate(testAgent, inputWidth(narrow), "…"); got != want {
		t.Fatalf("at width %d the info row is %q, want the agent name cut to %q", narrow, got, want)
	}
	want := ansi.Truncate(speaker+" · person", narrow-replyIndent, "…")
	found := false
	for _, l := range d.transcriptLines() {
		found = found || strings.TrimSpace(strings.TrimPrefix(l, "┃")) == want
	}
	if !found || !d.hasSentPanel("Hi") {
		t.Fatalf("at width %d the transcript does not show %q above the message:\n%s", narrow, want, d.transcript())
	}

	for _, size := range []tea.WindowSizeMsg{{Width: 4, Height: 8}, {Width: 1, Height: 7}, {Width: 1, Height: 2}} {
		d.update(size)
		v := d.view()
		lines := strings.Split(v, "\n")
		shown := lines[:min(len(lines), size.Height)]
		input := -1
		for i, l := range shown {
			if isPanelRow(strings.TrimLeft(l, " ")) && panelText(l) == ansi.Truncate(placeholder, inputWidth(size.Width), "") {
				input = i
			}
		}
		if len(lines) > size.Height || input < 0 || panelTop(t, shown) < 1 {
			t.Fatalf("at %dx%d the window's %d rows lost the transcript or the input row (view has %d lines):\n%s", size.Width, size.Height, size.Height, len(lines), v)
		}
	}
}
