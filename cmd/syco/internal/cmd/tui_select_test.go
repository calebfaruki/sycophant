package cmd

import (
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

// selectableTUI opens the TUI on a sent message and a reply, and records
// what the model copies instead of writing the clipboard.
func selectableTUI(t *testing.T) (*tuiDriver, *[]string) {
	t.Helper()
	f, _ := newClientEnv(t)
	f.setHistory(userEntry("SelectFirstHeron"), assistantEntry("SelectLastEgret is the reply"))
	d := openTUI(t, f)
	f.nextStream(t, 5*time.Second)
	d.waitConnected()
	d.waitView("the conversation", time.Now().Add(5*time.Second), func(string) bool {
		return rowIndex(d.transcriptLines(), "SelectLastEgret") >= 0
	})
	var copies []string
	d.model.(*tuiModel).copyText = func(s string) tea.Cmd {
		copies = append(copies, s)
		return nil
	}
	return d, &copies
}

// cellOf is the screen cell where s starts.
func (d *tuiDriver) cellOf(s string) (x, y int) {
	d.t.Helper()
	lines := strings.Split(d.view(), "\n")
	y = rowIndex(lines, s)
	if y < 0 {
		d.t.Fatalf("%q is not on screen:\n%s", s, d.view())
	}
	return ansi.StringWidth(lines[y][:strings.Index(lines[y], s)]), y
}

func TestTUIDragAcrossLinesHighlightsThemAndCopiesTheirPlainTextOnRelease(t *testing.T) {
	d, copies := selectableTUI(t)
	_, top := d.cellOf("SelectFirstHeron")
	_, bottom := d.cellOf("SelectLastEgret")
	plainTop, plainBottom := d.rawLineWith("SelectFirstHeron"), d.rawLineWith("SelectLastEgret")

	d.update(tea.MouseClickMsg{X: 0, Y: top, Button: tea.MouseLeft})
	d.update(tea.MouseMotionMsg{X: 159, Y: bottom, Button: tea.MouseLeft})
	for _, c := range []struct{ text, plain string }{{"SelectFirstHeron", plainTop}, {"SelectLastEgret", plainBottom}} {
		if raw := d.rawLineWith(c.text); raw == c.plain {
			t.Fatalf("the row holding %q is not highlighted while dragging:\n%q", c.text, raw)
		}
	}
	if len(*copies) != 0 {
		t.Fatalf("copied before the release: %q", *copies)
	}

	d.update(tea.MouseReleaseMsg{X: 159, Y: bottom, Button: tea.MouseLeft})
	want := "SelectFirstHeron\n\n\na-test · main\nSelectLastEgret is the reply"
	if len(*copies) != 1 || (*copies)[0] != want {
		t.Fatalf("release copied %q, want [%q]", *copies, want)
	}
	if d.rawLineWith("SelectFirstHeron") != plainTop || d.rawLineWith("SelectLastEgret") != plainBottom {
		t.Fatal("the highlight stayed after the release")
	}
}

func TestTUIDragWithinALineCopiesOnlyTheCellsUnderItInEitherDirection(t *testing.T) {
	d, copies := selectableTUI(t)
	x, y := d.cellOf("Heron")
	end := x + len("Heron") - 1

	d.update(tea.MouseClickMsg{X: x, Y: y, Button: tea.MouseLeft})
	d.update(tea.MouseMotionMsg{X: end, Y: y, Button: tea.MouseLeft})
	d.update(tea.MouseReleaseMsg{X: end, Y: y, Button: tea.MouseLeft})
	d.update(tea.MouseClickMsg{X: end, Y: y, Button: tea.MouseLeft})
	d.update(tea.MouseMotionMsg{X: x, Y: y, Button: tea.MouseLeft})
	d.update(tea.MouseReleaseMsg{X: x, Y: y, Button: tea.MouseLeft})

	if want := []string{"Heron", "Heron"}; strings.Join(*copies, "|") != strings.Join(want, "|") {
		t.Fatalf("copied %q, want %q", *copies, want)
	}
}

func TestTUIClickWithoutADragCopiesNothingAndLeavesNoHighlight(t *testing.T) {
	d, copies := selectableTUI(t)
	x, y := d.cellOf("Heron")
	plain := d.rawLineWith("Heron")

	d.update(tea.MouseClickMsg{X: x, Y: y, Button: tea.MouseLeft})
	d.update(tea.MouseReleaseMsg{X: x, Y: y, Button: tea.MouseLeft})

	if len(*copies) != 0 {
		t.Fatalf("a click copied %q", *copies)
	}
	if got := d.rawLineWith("Heron"); got != plain {
		t.Fatalf("a click left the row styled differently:\nbefore %q\nafter  %q", plain, got)
	}
}

func TestTUIDragFromTheInputPanelOrTheStartScreenCopiesNothing(t *testing.T) {
	d, copies := selectableTUI(t)
	_, top := d.cellOf("SelectFirstHeron")
	panel := d.model.(*tuiModel).viewport.Height()
	d.update(tea.MouseClickMsg{X: 0, Y: panel, Button: tea.MouseLeft})
	d.update(tea.MouseMotionMsg{X: 159, Y: top, Button: tea.MouseLeft})
	d.update(tea.MouseReleaseMsg{X: 159, Y: top, Button: tea.MouseLeft})
	if len(*copies) != 0 {
		t.Fatalf("a drag from the input panel copied %q", *copies)
	}

	_, start, _ := connectedTUI(t)
	start.update(tea.WindowSizeMsg{Width: 80, Height: 40})
	start.model.(*tuiModel).copyText = func(s string) tea.Cmd {
		*copies = append(*copies, s)
		return nil
	}
	start.update(tea.MouseClickMsg{X: 0, Y: 0, Button: tea.MouseLeft})
	start.update(tea.MouseMotionMsg{X: 79, Y: 20, Button: tea.MouseLeft})
	start.update(tea.MouseReleaseMsg{X: 79, Y: 20, Button: tea.MouseLeft})
	if len(*copies) != 0 {
		t.Fatalf("a drag on the start screen copied %q", *copies)
	}
}
