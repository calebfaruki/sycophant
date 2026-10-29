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

func TestTUILineUnderThePanelShowsWorkingAndEscStopWhileATurnRunsAndIdentityServerAndNewlineWhenIdle(t *testing.T) {
	const conv = "conv-footer"
	f, d, stream := tuiTurn(t, conv)
	if left, hints := d.footerParts(); left != working || hints != "esc stop" {
		t.Fatalf("line under the panel while the turn runs is %q / %q, want %q / %q:\n%s", left, hints, working, "esc stop", d.view())
	}

	d.finishTurn(stream, conv)
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
	_, d, stream := tuiTurn(t, "conv-bar-colour")
	if raw := d.model.View().Content; !barStarts(raw, panelAccent) {
		t.Fatalf("panel bar is not drawn in the accent colour:\n%q", raw)
	}
	stream.push(approvalRequest(t, "req-bar"))
	d.waitMsg("the approval request frame", 5*time.Second, isServerRequestFrame)
	if raw := d.model.View().Content; !barStarts(raw, panelApproval) {
		t.Fatalf("panel bar is not yellow while an approval waits:\n%q", raw)
	}

	off := newTUIModel(t.TempDir(), &config.Profile{Server: "bar.invalid", Workspace: "w-bar"}, "", false, "", nil)
	off.Update(tea.WindowSizeMsg{Width: 80, Height: 20})
	if raw := off.View().Content; !barStarts(raw, panelDisconnected) {
		t.Fatalf("panel bar is not red while disconnected:\n%q", raw)
	}
}

func TestTUIApprovalShowsInsideThePanelInPlaceOfTheInput(t *testing.T) {
	_, d, stream := tuiTurn(t, "conv-approval-panel")
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
	_, d, _ := connectedTUI(t, "conv-fill")
	d.update(tea.WindowSizeMsg{Width: 80, Height: height})
	// The first message leaves the start screen for the chat layout.
	d.send("UserFillFirstAuk")
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
	if got, want := panelText(lines[top+3]), "Default · "+testWorkspace; got != want {
		t.Fatalf("info row is %q, want %q", got, want)
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
