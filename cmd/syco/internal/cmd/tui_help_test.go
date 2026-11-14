// The tests in this file cover keyboard shortcut help.

package cmd

import (
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

func TestTUIF1OpensAndClosesKeyboardHelpAndEscTakesPrecedence(t *testing.T) {
	f, d, _ := tuiTurn(t, "conv-help-toggle")
	m := d.model.(*tuiModel)

	d.update(keyPress(tea.KeyF1))
	if !m.helpOpen || !strings.Contains(d.view(), "Keyboard shortcuts") {
		t.Fatalf("F1 did not open keyboard help:\n%s", d.view())
	}

	d.update(keyPress(tea.KeyF1))
	if m.helpOpen {
		t.Fatal("a second F1 did not close keyboard help")
	}

	d.update(keyPress(tea.KeyF1))
	d.update(keyPress(tea.KeyEscape))
	if m.helpOpen || m.stopping || m.quitting {
		t.Fatalf("Esc from help left help=%v stopping=%v quitting=%v",
			m.helpOpen, m.stopping, m.quitting)
	}
	f.noCancel(t, 300*time.Millisecond)
}

func TestTUIHelpPreservesDraftBlocksInputAndRestoresFocus(t *testing.T) {
	f, d, _ := connectedTUI(t, "conv-help-draft")
	m := d.model.(*tuiModel)

	d.update(keyText("DraftCapybara"))
	d.update(keyPress(tea.KeyF1))
	if m.input.Focused() {
		t.Fatal("input stayed focused while keyboard help was open")
	}

	d.update(keyText("IgnoredOtter"))
	d.update(tea.PasteMsg{Content: "IgnoredPasteMink"})
	d.update(keyPress(tea.KeyEnter))
	d.wantInput("typing paste and Enter while help was open", "DraftCapybara")
	if got := len(f.ingestLog()); got != 0 {
		t.Fatalf("help allowed %d message ingests, want 0", got)
	}

	d.update(keyPress(tea.KeyF1))
	d.wantInput("closing help", "DraftCapybara")
	if !m.input.Focused() {
		t.Fatal("input focus was not restored after help closed")
	}
}

func TestTUICtrlCStillQuitsWhileKeyboardHelpIsOpen(t *testing.T) {
	_, d, _ := connectedTUI(t, "conv-help-quit")
	d.update(keyPress(tea.KeyF1))
	d.update(keyPress('c', tea.ModCtrl))
	d.waitMsg("the quit", time.Second, func(msg tea.Msg) bool {
		_, ok := msg.(tea.QuitMsg)
		return ok
	})
}

func TestTUIKeyboardHelpListsControlsAndFitsNarrowScreen(t *testing.T) {
	_, d, _ := connectedTUI(t, "conv-help-narrow")
	const width, height = 44, 24
	d.update(tea.WindowSizeMsg{Width: width, Height: height})
	d.update(keyPress(tea.KeyF1))

	view := d.view()
	for _, want := range []string{
		"Keyboard shortcuts",
		"Enter",
		"Shift+Enter",
		"Ctrl+J / Alt+Enter",
		"↑ / ↓",
		"Esc",
		"Ctrl+C",
		"F1",
	} {
		if !strings.Contains(view, want) {
			t.Fatalf("narrow help is missing %q:\n%s", want, view)
		}
	}

	lines := strings.Split(view, "\n")
	if len(lines) > height {
		t.Fatalf("narrow help is %d lines high, want at most %d:\n%s",
			len(lines), height, view)
	}
	for _, line := range lines {
		if got := ansi.StringWidth(line); got > width {
			t.Fatalf("help line %q is %d columns wide, want at most %d",
				line, got, width)
		}
	}
}
