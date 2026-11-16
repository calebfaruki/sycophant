// The tests in this file cover the mascot on the start screen and in the
// conversation view, and the minimal setting that hides it.

package cmd

import (
	"fmt"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/sycophant-md/sycophant/cmd/syco/internal/config"
)

const imageCell = "\U0010EEEE"

const testCellW, testCellH = 8, 16

// initRaw is what m's Init writes to the terminal unprocessed.
func initRaw(t *testing.T, m *tuiModel) string {
	t.Helper()
	var raw strings.Builder
	var collect func(tea.Cmd)
	collect = func(cmd tea.Cmd) {
		if cmd == nil {
			return
		}
		got := make(chan tea.Msg, 1)
		go func() { got <- cmd() }()
		select {
		case msg := <-got:
			switch msg := msg.(type) {
			case tea.BatchMsg:
				for _, c := range msg {
					collect(c)
				}
			case tea.RawMsg:
				raw.WriteString(fmt.Sprint(msg.Msg))
			case connectedMsg:
				msg.conn.Close()
			}
		case <-time.After(5 * time.Second):
			t.Fatal("an Init command did not return within 5s")
		}
	}
	collect(m.Init())
	return raw.String()
}

func TestTUISendsBothMascotImagesAtStartOnlyWhenTheTerminalShowsImages(t *testing.T) {
	newClientEnv(t)
	cfgDir, p, token := testProfile(t)

	off := newTUIModel(cfgDir, p, token)
	if raw := initRaw(t, off); strings.Contains(raw, "\x1b_G") {
		t.Fatalf("start without images sent image data: %q", raw)
	}

	on := newTUIModel(cfgDir, p, token)
	on.enableImages(testCellW, testCellH)
	_, picking, _ := testProfile(t)
	picking.Agent = ""
	picker := newTUIModel(cfgDir, picking, token)
	picker.enableImages(testCellW, testCellH)
	for screen, m := range map[string]*tuiModel{"the chat": on, "the picker": picker} {
		raw := initRaw(t, m)
		for name, upload := range map[string]string{"start screen": m.startMascot.upload, "chat": m.chatMascot.upload} {
			if !strings.Contains(upload, "\x1b_G") || !strings.Contains(raw, upload) {
				t.Fatalf("start on %s with images did not send the %s mascot image; sent %q", screen, name, raw)
			}
		}
	}
}

func TestTUIShowsNoImagesWhenTheStartMascotIsTallerThanAnImageCanBe(t *testing.T) {
	m := &tuiModel{profile: &config.Profile{}}
	m.enableImages(8, 4)
	if m.cuteMode() {
		t.Fatalf("images are on with a %d-row start mascot, more than the %d an image can span", m.startMascot.rows, len(diacritics))
	}
}

func TestTUIStartScreenShowsTheMascotLeftOfTheLogoFlushWithItsBottomOnlyWhileTheWindowHasRoomForIt(t *testing.T) {
	_, d, _ := connectedTUI(t)
	d.update(tea.WindowSizeMsg{Width: 80, Height: 40})
	m := d.model.(*tuiModel)
	logoLines := logoRows()

	// The start screen holds the mascot beside the logo only while the
	// window, less its corner row, has room for the taller of the two, a
	// blank row and the panel with its footer. noRoom is the first height
	// short of that.
	probe := &tuiModel{}
	probe.enableImages(testCellW, testCellH)
	pw := m.panelWidth()
	group := strings.Count(m.viewPanel(pw)+"\n"+m.viewFooter(pw), "\n") + 1
	noRoom := max(probe.startMascot.rows, len(logoLines)) + 1 + group
	d.update(tea.WindowSizeMsg{Width: 80, Height: noRoom})
	without := d.model.View().Content

	m.enableImages(testCellW, testCellH)
	cols, rows := m.startMascot.cols, m.startMascot.rows
	logoW := lipgloss.Width(logo)
	lead := (80 - (cols + 1 + logoW)) / 2
	wantMascot := func(when string) {
		t.Helper()
		v := d.view()
		lines := strings.Split(v, "\n")
		top := rowIndex(lines, logoLines[0])
		bottom := top + len(logoLines) - 1
		if top < 0 || bottom < rows-1 {
			t.Fatalf("%s: logo at line %d leaves no room for %d mascot rows ending on its bottom row:\n%s", when, top, rows, v)
		}
		for i, l := range lines {
			inMascot := i > bottom-rows && i <= bottom
			if strings.Contains(l, imageCell) != inMascot {
				t.Fatalf("%s: line %d is %q; want the mascot on the %d lines ending on the logo's bottom row only:\n%s", when, i, l, rows, v)
			}
			if !inMascot {
				continue
			}
			if at := strings.Index(l, imageCell); ansi.StringWidth(l[:at]) != lead {
				t.Fatalf("%s: mascot row %q does not start at column %d, the mascot and logo centred on 80 columns", when, l, lead)
			}
			if i < top {
				continue
			}
			if at := strings.Index(l, logoLines[i-top]); at < 0 || ansi.StringWidth(l[:at]) != lead+cols+1 {
				t.Fatalf("%s: line %q does not hold logo row %d one column right of the mascot", when, l, i-top)
			}
		}
	}

	d.update(tea.WindowSizeMsg{Width: 80, Height: 40})
	wantMascot("at 80x40")
	d.update(tea.WindowSizeMsg{Width: 80, Height: noRoom})
	if got := d.model.View().Content; got != without {
		t.Fatalf("at 80x%d the start screen differs from the one without images:\n%s\nwant:\n%s", noRoom, ansi.Strip(got), ansi.Strip(without))
	}
	if !strings.Contains(ansi.Strip(without), logoLines[0]) {
		t.Fatalf("at 80x%d the logo is gone too, want the logo alone:\n%s", noRoom, ansi.Strip(without))
	}
	d.update(tea.WindowSizeMsg{Width: 80, Height: 40})
	wantMascot("back at 80x40")
}

// barColumn is the screen column of the first of marks in line, or -1.
func barColumn(line string, marks ...string) int {
	for _, mark := range marks {
		if i := strings.Index(line, mark); i >= 0 {
			return ansi.StringWidth(line[:i])
		}
	}
	return -1
}

// afterBar is a panel row's text right of its bar.
func afterBar(line string) string {
	i := strings.Index(line, "┃")
	if i < 0 {
		return ""
	}
	return strings.TrimSpace(line[i+len("┃"):])
}

// The panel is its top padding, one input row, a blank row, the info row
// and its bottom edge. The footer is the view's last line.
func TestTUIConversationShowsTheMascotLeftOfThePanelFlushWithItsBottomEdge(t *testing.T) {
	_, d, _ := startedTUI(t, "UserMascotChatIbis")
	m := d.model.(*tuiModel)
	m.enableImages(testCellW, testCellH)
	rows := m.chatMascot.rows

	wantLayout := func(width int, mascot bool) {
		t.Helper()
		d.update(tea.WindowSizeMsg{Width: width, Height: 12})
		v := d.view()
		lines := strings.Split(v, "\n")
		edge := len(lines) - 2
		col := 0
		if mascot {
			col = m.chatMascot.cols
		}
		for i, l := range lines {
			inMascot := mascot && i > edge-rows && i <= edge
			if inMascot != strings.HasPrefix(l, imageCell) || (!inMascot && strings.Contains(l, imageCell)) {
				t.Fatalf("at width %d line %d is %q; want the mascot left of the panel in the %d rows ending on its edge only:\n%s", width, i, l, rows, v)
			}
		}
		for _, l := range lines[edge-4 : edge+1] {
			if got := barColumn(l, "┃", "╹"); got != col {
				t.Fatalf("at width %d panel row %q starts at column %d, want %d:\n%s", width, l, got, col, v)
			}
			if w := ansi.StringWidth(l); w != width {
				t.Fatalf("at width %d panel row %q is %d wide, want the screen's %d", width, l, w, width)
			}
		}
	}

	wantLayout(80, true)
	wantLayout(20, false)
	wantLayout(80, true)
}

func TestTUIConversationWithTheMascotIsAsTallAsTheWindow(t *testing.T) {
	_, d, _ := startedTUI(t, "UserMascotHeightIbis")
	d.model.(*tuiModel).enableImages(testCellW, testCellH)
	d.update(tea.WindowSizeMsg{Width: 80, Height: 12})
	v := d.view()
	if n := len(strings.Split(v, "\n")); n != 12 || !strings.Contains(v, imageCell) {
		t.Fatalf("at 80x12 with a one-line input the view is %d lines, want 12 with the mascot shown:\n%s", n, v)
	}
}

// The mascot takes its columns from the panel and the footer only while the
// agent name under the input and the identity line in the footer both stay
// whole. Neither is cut at either width.
func TestTUIConversationDropsTheMascotOnlyOnceKeepingItWouldCutTheAgentNameOrTheIdentityLine(t *testing.T) {
	f, d, _ := startedTUI(t, "UserMascotRoomIbis")
	m := d.model.(*tuiModel)
	m.enableImages(testCellW, testCellH)
	identity := "tester · " + f.addr

	check := func(agent string, need int) {
		t.Helper()
		m.profile.Agent = agent
		for _, c := range []struct {
			width  int
			mascot bool
		}{{m.chatMascot.cols + need, true}, {m.chatMascot.cols + need - 1, false}} {
			d.update(tea.WindowSizeMsg{Width: c.width, Height: 12})
			v := d.view()
			if got := strings.Contains(v, imageCell); got != c.mascot {
				t.Fatalf("agent %q at width %d: mascot shown is %v, want %v:\n%s", agent, c.width, got, c.mascot, v)
			}
			lines := strings.Split(v, "\n")
			edge := len(lines) - 2
			if got := afterBar(lines[edge-1]); got != agent {
				t.Fatalf("agent %q at width %d: info row is %q, want the whole name:\n%s", agent, c.width, got, v)
			}
			if got := footerGapRe.Split(strings.TrimSpace(lines[edge+1]), 2)[0]; got != identity {
				t.Fatalf("agent %q at width %d: footer reads %q, want the whole identity line %q:\n%s", agent, c.width, got, identity, v)
			}
		}
	}

	// The identity line is the wider: it decides.
	check(testAgent, ansi.StringWidth(identity)+2*panelPad)
	// An agent name wider than the identity line decides.
	const long = "AgentNameWiderThanTheIdentityLineHeron"
	check(long, len(long)+1+2*panelPad)
}

func TestTUIMinimalShowsNoMascotAndSendsNoImageInATerminalThatShowsImages(t *testing.T) {
	f, _ := newClientEnv(t)
	cfgDir, p, token := testProfile(t)
	p.Minimal = true
	m := newTUIModel(cfgDir, p, token)
	m.enableImages(testCellW, testCellH)
	d := newTUIDriver(t, m)
	d.f = f
	d.run(m.Init())
	d.update(tea.WindowSizeMsg{Width: 80, Height: 40})
	stream := f.nextStream(t, 5*time.Second)
	d.waitConnected()

	if v := d.view(); logoAt(strings.Split(v, "\n")) < 0 || strings.Contains(v, imageCell) {
		t.Fatalf("minimal start screen at 80x40 shows a mascot or lost the logo:\n%s", v)
	}

	d.send("UserMinimalHeron")
	d.startTurn(stream, "msg-minimal", "UserMinimalHeron")
	d.update(tea.WindowSizeMsg{Width: 80, Height: 12})
	v := d.view()
	lines := strings.Split(v, "\n")
	if strings.Contains(v, imageCell) || barColumn(lines[len(lines)-6], "┃") != 0 {
		t.Fatalf("minimal conversation view shows a mascot or moves the chat bar off column 0:\n%s", v)
	}

	if raw := initRaw(t, m); strings.Contains(raw, "\x1b_G") {
		t.Fatalf("minimal start sent image data: %q", raw)
	}
}

func TestChatHelpDescribesMinimal(t *testing.T) {
	help := runPrinting(t, "chat", "--help")
	for _, l := range strings.Split(help, "\n") {
		if _, desc, ok := strings.Cut(l, "--minimal"); ok {
			if strings.TrimSpace(desc) == "" {
				t.Fatalf("syco chat --help lists --minimal with no description: %q", l)
			}
			return
		}
	}
	t.Fatalf("syco chat --help does not list --minimal:\n%s", help)
}
