// The tests in this file cover the start screen a new conversation opens
// on, until the first message switches to the chat layout.

package cmd

import (
	"image/color"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
)

func dot(c color.Color) string {
	return lipgloss.NewStyle().Foreground(c).Render("●")
}

func TestTUIStartScreenShowsTheLogoAndTheCentredPanelWithTheLineUnderItAndTheCorners(t *testing.T) {
	f, d, _ := connectedTUI(t, "conv-start")
	d.update(tea.WindowSizeMsg{Width: 80, Height: 24})
	v := d.view()
	lines := strings.Split(v, "\n")
	if len(lines) != 24 {
		t.Fatalf("view is %d lines, want the screen's 24:\n%s", len(lines), v)
	}

	at := logoAt(lines)
	if at < 0 {
		t.Fatalf("no logo on the start screen:\n%s", v)
	}
	for i, want := range logoRows() {
		if got := lines[at+i]; strings.TrimRight(got, " ") != strings.Repeat(" ", (80-lipgloss.Width(logo))/2)+want {
			t.Fatalf("logo row %d is %q, want it centred on 80 columns:\n%s", i, got, v)
		}
	}
	h := len(logoRows())
	if !isBlank(lines[at+h]) {
		t.Fatalf("line under the logo is %q, want blank", lines[at+h])
	}

	top := at + h + 1
	edge := panelEdge(t, lines)
	// top padding, the input, a blank row, the info line.
	if edge != top+4 {
		t.Fatalf("panel runs from line %d to its edge at %d, want a one-line input panel right under the logo's blank line:\n%s", top, edge, v)
	}
	for i := top; i <= edge; i++ {
		l := lines[i]
		w := ansi.StringWidth(strings.TrimLeft(l, " "))
		if textColumn(l) != (80-startPanelMax)/2 || w != startPanelMax {
			t.Fatalf("panel row %d starts at column %d and is %d wide, want %d wide centred:\n%s", i, textColumn(l), w, startPanelMax, v)
		}
	}
	if got := panelText(lines[top+1]); got != placeholder {
		t.Fatalf("panel's input row is %q, want the placeholder", got)
	}

	above := at
	below := 23 - (edge + 2) // blank rows between the group and the corners row
	for _, l := range append(append([]string{}, lines[:at]...), lines[edge+2:23]...) {
		if !isBlank(l) {
			t.Fatalf("line %q sits outside the start screen's group:\n%s", l, v)
		}
	}
	if above-below > 1 || below-above > 1 {
		t.Fatalf("%d blank lines above the group and %d below, want them within one:\n%s", above, below, v)
	}

	corners := lines[23]
	if !strings.HasPrefix(strings.TrimSpace(corners), "● "+f.addr) || !strings.HasSuffix(strings.TrimSpace(corners), version) {
		t.Fatalf("last row is %q, want the dot and server left and %s right", corners, version)
	}
}

func TestTUIStartScreenDropsTheLogoWhenShortAndNeverWrapsItWhenNarrow(t *testing.T) {
	_, d, _ := connectedTUI(t, "conv-start-small")
	for _, size := range []tea.WindowSizeMsg{{Width: 80, Height: 10}, {Width: 40, Height: 30}} {
		d.update(size)
		v := d.view()
		lines := strings.Split(v, "\n")
		if len(lines) != size.Height {
			t.Fatalf("at %dx%d the view is %d lines, want %d:\n%s", size.Width, size.Height, len(lines), size.Height, v)
		}
		for _, r := range logoRows() {
			if strings.Contains(v, r) {
				t.Fatalf("at %dx%d the logo (or part of it) still shows:\n%s", size.Width, size.Height, v)
			}
		}
		for i, l := range lines {
			if w := ansi.StringWidth(l); w > size.Width {
				t.Fatalf("at %dx%d line %d is %d wide: %q", size.Width, size.Height, i, w, l)
			}
		}
		if !strings.Contains(v, placeholder) || !strings.Contains(lines[len(lines)-1], "●") {
			t.Fatalf("at %dx%d the panel or corners are missing:\n%s", size.Width, size.Height, v)
		}
	}
}

func TestTUIDroppedStreamShowsDisconnectedUntilItReconnects(t *testing.T) {
	f, d, stream := connectedTUI(t, "conv-start-drop")
	d.update(tea.WindowSizeMsg{Width: 80, Height: 24})
	d.fastRetries()

	stream.end()
	d.waitView("disconnected after the stream drops", time.Now().Add(5*time.Second), func(string) bool {
		return d.footerLeft() == "disconnected"
	})
	raw := d.model.View().Content
	if !strings.Contains(raw, dot(panelDisconnected)) {
		t.Fatalf("connection dot is not red after the stream dropped:\n%q", raw)
	}

	d.waitView("connected again after the reconnect", time.Now().Add(10*time.Second), func(string) bool {
		return d.footerLeft() != "disconnected"
	})
	f.nextStream(t, time.Second)
	raw = d.model.View().Content
	if !strings.Contains(raw, dot(connectedDot)) {
		t.Fatalf("connection dot is not back to green after the reconnect:\n%q", raw)
	}
}
