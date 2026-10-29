// The helpers in this file drive the chat TUI in tests and read its screen.

package cmd

import (
	"encoding/json"
	"fmt"
	"image/color"
	"regexp"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/sycophant-md/sycophant/cmd/syco/internal/config"
	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

// tuiDriver runs a TUI model without a terminal, the way the program would:
// every command runs on its own goroutine and its message comes back to
// Update on the test goroutine, one at a time.
type tuiDriver struct {
	t     *testing.T
	model tea.Model
	msgs  chan tea.Msg
	done  chan struct{}
}

// newTUIDriver drives m until the test ends.
func newTUIDriver(t *testing.T, m tea.Model) *tuiDriver {
	d := &tuiDriver{t: t, model: m, msgs: make(chan tea.Msg, 256), done: make(chan struct{})}
	t.Cleanup(func() { close(d.done) })
	return d
}

func (d *tuiDriver) run(cmd tea.Cmd) {
	if cmd == nil {
		return
	}
	go func() { d.post(cmd()) }()
}

func (d *tuiDriver) post(msg tea.Msg) {
	if msg == nil {
		return
	}
	if batch, ok := msg.(tea.BatchMsg); ok {
		for _, c := range batch {
			d.run(c)
		}
		return
	}
	select {
	case d.msgs <- msg:
	case <-d.done:
	}
}

func (d *tuiDriver) update(msg tea.Msg) {
	var cmd tea.Cmd
	d.model, cmd = d.model.Update(msg)
	d.run(cmd)
}

// view is the rendered view with ANSI escapes stripped.
func (d *tuiDriver) view() string {
	return ansi.Strip(d.model.View().Content)
}

// waitView feeds messages to the model until cond holds for its view, or
// fails the test at the deadline.
func (d *tuiDriver) waitView(desc string, deadline time.Time, cond func(view string) bool) {
	d.t.Helper()
	for {
		if cond(d.view()) {
			return
		}
		left := time.Until(deadline)
		if left <= 0 {
			d.t.Fatalf("timed out waiting for %s; view:\n%s", desc, d.view())
		}
		select {
		case msg := <-d.msgs:
			d.update(msg)
		case <-time.After(min(left, 50*time.Millisecond)):
		}
	}
}

// waitMsg feeds messages to the model until it has handled one for which
// match holds.
func (d *tuiDriver) waitMsg(desc string, timeout time.Duration, match func(tea.Msg) bool) {
	d.t.Helper()
	deadline := time.After(timeout)
	for {
		select {
		case msg := <-d.msgs:
			d.update(msg)
			if match(msg) {
				return
			}
		case <-deadline:
			d.t.Fatalf("timed out waiting for %s; view:\n%s", desc, d.view())
		}
	}
}

func (d *tuiDriver) waitConnected() {
	d.t.Helper()
	d.waitView("the TUI to connect", time.Now().Add(5*time.Second), func(string) bool {
		return d.model.(*tuiModel).acceptsInput()
	})
}

// waitIngests feeds messages to the model until the fake has seen n ingests.
func (d *tuiDriver) waitIngests(f *fakeGateway, n int) {
	d.t.Helper()
	d.waitView(fmt.Sprintf("%d ingests", n), time.Now().Add(5*time.Second), func(string) bool {
		return len(f.ingestLog()) >= n
	})
}

// settle feeds the model every message that arrives within dur.
func (d *tuiDriver) settle(dur time.Duration) {
	deadline := time.After(dur)
	for {
		select {
		case msg := <-d.msgs:
			d.update(msg)
		case <-deadline:
			return
		}
	}
}

// openTUI builds the model through openTUIModel for the test profile and
// the given flags, runs Init, and sizes the window to show the whole
// transcript.
func openTUI(t *testing.T, continueFlag bool, resumeID string) *tuiDriver {
	t.Helper()
	cfgDir, p, token := testProfile(t)
	model, err := openTUIModel(cfgDir, p, token, continueFlag, resumeID, nil)
	if err != nil {
		t.Fatalf("openTUIModel: %v", err)
	}
	d := newTUIDriver(t, model)
	d.run(model.Init())
	d.update(tea.WindowSizeMsg{Width: 160, Height: 200})
	return d
}

func testProfile(t *testing.T) (cfgDir string, p *config.Profile, token string) {
	t.Helper()
	cfgDir = config.ConfigDir()
	p, err := config.LoadProfile(cfgDir, "", "")
	if err != nil {
		t.Fatal(err)
	}
	token, err = config.LoadToken(cfgDir, p.Name)
	if err != nil {
		t.Fatal(err)
	}
	return cfgDir, p, token
}

// connectedTUI opens the TUI on a fresh conversation and waits until it
// is connected.
func connectedTUI(t *testing.T, conv string) (*fakeGateway, *tuiDriver, *fakeStream) {
	t.Helper()
	f, _ := newClientEnv(t)
	f.setFreshID(conv)
	d := openTUI(t, false, "")
	stream := f.nextStream(t, 5*time.Second)
	d.waitConnected()
	return f, d, stream
}

// tuiTurn opens the TUI on a fresh conversation, sends a message, and marks
// the turn running.
func tuiTurn(t *testing.T, conv string) (*fakeGateway, *tuiDriver, *fakeStream) {
	t.Helper()
	f, d, stream := connectedTUI(t, conv)
	d.send("UserTurnLynx")
	stream.push(turnStateFrame(conv, commonpb.TurnState_TURN_STATE_WORKING))
	d.waitView("the working indicator", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, working)
	})
	return f, d, stream
}

// finishTurn ends the running turn IDLE and waits for the idle hint.
func (d *tuiDriver) finishTurn(s *fakeStream, conv string) {
	d.t.Helper()
	s.push(turnStateFrame(conv, commonpb.TurnState_TURN_STATE_IDLE))
	d.waitView("the turn to end", time.Now().Add(5*time.Second), func(v string) bool {
		return !strings.Contains(v, working) && strings.Contains(v, "shift+enter newline")
	})
}

// fastRetries makes each second of a reconnect wait last a millisecond.
func (d *tuiDriver) fastRetries() {
	d.model.(*tuiModel).retryTick = time.Millisecond
}

// dropAndReconnect ends s and waits until the TUI has dropped and
// reconnected on a new stream.
func (d *tuiDriver) dropAndReconnect(f *fakeGateway, s *fakeStream) {
	d.t.Helper()
	d.fastRetries()
	s.end()
	d.waitView("the drop", time.Now().Add(5*time.Second), func(string) bool {
		return strings.HasPrefix(d.footerLeft(), "disconnected")
	})
	d.waitView("the reconnect", time.Now().Add(5*time.Second), func(string) bool {
		return strings.HasPrefix(d.footerLeft(), "tester · ")
	})
	f.nextStream(d.t, time.Second)
}

// transcript is the view above the panel.
func (d *tuiDriver) transcript() string { return strings.Join(d.transcriptLines(), "\n") }

func (d *tuiDriver) transcriptLines() []string {
	lines := strings.Split(d.view(), "\n")
	return lines[:panelTop(d.t, lines)]
}

// hasSentPanel reports whether the transcript shows text as a row of a
// sent message's panel.
func (d *tuiDriver) hasSentPanel(text string) bool {
	for _, l := range d.transcriptLines() {
		if isPanelRow(l) && panelRowText(l) == text {
			return true
		}
	}
	return false
}

func (d *tuiDriver) inputValue() string {
	return d.model.(*tuiModel).input.Value()
}

// panelTop is the index of the panel's first row (its top padding) in the
// view's lines: the first row of the bottom block drawn with the bar.
func panelTop(t *testing.T, lines []string) int {
	t.Helper()
	top := -1
	for i := len(lines) - 1; i >= 0; i-- {
		if strings.HasPrefix(strings.TrimLeft(lines[i], " "), "┃") {
			top = i
		} else if top >= 0 {
			break
		}
	}
	if top < 0 {
		t.Fatalf("no panel in view:\n%s", strings.Join(lines, "\n"))
	}
	return top
}

// panelEdge is the index of the panel's bottom edge row.
func panelEdge(t *testing.T, lines []string) int {
	t.Helper()
	for i := len(lines) - 1; i >= 0; i-- {
		if strings.HasPrefix(strings.TrimLeft(lines[i], " "), "╹") {
			return i
		}
	}
	t.Fatalf("no panel edge in view:\n%s", strings.Join(lines, "\n"))
	return -1
}

// panelText is a panel row without its bar and padding.
func panelText(row string) string {
	return strings.TrimSpace(strings.TrimPrefix(strings.TrimLeft(row, " "), "┃"))
}

func isPanelRow(line string) bool { return strings.HasPrefix(line, "┃") }

// panelRowText is a message panel row's text: after the bar and its two
// blanks, trailing blanks dropped.
func panelRowText(line string) string {
	return strings.TrimRight(strings.TrimPrefix(strings.TrimPrefix(line, "┃"), "  "), " ")
}

func textColumn(line string) int {
	for i, r := range []rune(line) {
		if r != ' ' {
			return i
		}
	}
	return -1
}

func rowIndex(lines []string, s string) int {
	for i, l := range lines {
		if strings.Contains(l, s) {
			return i
		}
	}
	return -1
}

// footer is the line under the panel.
func (d *tuiDriver) footer() string {
	lines := strings.Split(d.view(), "\n")
	return strings.TrimSpace(lines[panelEdge(d.t, lines)+1])
}

var footerGapRe = regexp.MustCompile(` {2,}`)

// footerParts splits the line under the panel into its status (left) and
// key hints (right), which a run of blanks separates.
func (d *tuiDriver) footerParts() (left, hints string) {
	parts := footerGapRe.Split(d.footer(), 2)
	if len(parts) == 2 {
		return parts[0], parts[1]
	}
	return parts[0], ""
}

func (d *tuiDriver) footerLeft() string {
	left, _ := d.footerParts()
	return left
}

// rawLineWith is the first line of the styled view whose text holds s.
func (d *tuiDriver) rawLineWith(s string) string {
	for _, l := range strings.Split(d.model.View().Content, "\n") {
		if strings.Contains(ansi.Strip(l), s) {
			return l
		}
	}
	return ""
}

// logoRows are the logo's rows without trailing blanks.
func logoRows() []string {
	rows := strings.Split(logo, "\n")
	for i, r := range rows {
		rows[i] = strings.TrimRight(r, " ")
	}
	return rows
}

func logoAt(lines []string) int {
	first := logoRows()[0]
	for i, l := range lines {
		if strings.TrimSpace(l) == first {
			return i
		}
	}
	return -1
}

// barStarts reports whether the raw view's input panel rows (the bottom
// block of rows drawn with the bar; message panels in the transcript keep
// the accent) all start with the bar drawn in c.
func barStarts(view string, c color.Color) bool {
	lines := strings.Split(view, "\n")
	want := panelBar(c)
	n := 0
	for i := len(lines) - 1; i >= 0; i-- {
		l := lines[i]
		if strings.HasPrefix(strings.TrimLeft(ansi.Strip(l), " "), "┃") {
			if !strings.HasPrefix(strings.TrimLeft(l, " "), want) {
				return false
			}
			n++
		} else if n > 0 {
			break
		}
	}
	return n > 0
}

const working = "working…"

const placeholder = "Ask anything"

func keyText(text string) tea.KeyPressMsg {
	code := tea.KeyExtended
	if r := []rune(text); len(r) == 1 {
		code = r[0]
	}
	return tea.KeyPressMsg{Code: code, Text: text}
}

// keyPress is the press of a key that types nothing, such as Enter or Up.
func keyPress(code rune, mod ...tea.KeyMod) tea.KeyPressMsg {
	k := tea.KeyPressMsg{Code: code}
	for _, m := range mod {
		k.Mod |= m
	}
	return k
}

// send types text into the input and presses Enter once the TUI is
// connected, then waits until the TUI has handled the ingest ack.
func (d *tuiDriver) send(text string) {
	d.t.Helper()
	d.waitConnected()
	d.typeEnter(text)
	d.waitMsg("the ingest ack", 5*time.Second, isIngestAck)
}

// typeEnter types text into the input and presses Enter without waiting
// for anything.
func (d *tuiDriver) typeEnter(text string) {
	d.update(keyText(text))
	d.update(keyPress(tea.KeyEnter))
}

func (d *tuiDriver) typeLines(lines ...string) {
	for i, l := range lines {
		if i > 0 {
			d.update(keyPress('j', tea.ModCtrl))
		}
		d.update(keyText(l))
	}
}

func (d *tuiDriver) wantInput(after, want string) {
	d.t.Helper()
	if got := d.inputValue(); got != want {
		d.t.Fatalf("after %s the input holds %q, want %q", after, got, want)
	}
}

func isIngestAck(msg tea.Msg) bool {
	_, ok := msg.(ingestAckMsg)
	return ok
}

func ingestText(req *commonpb.ChannelStreamIngestRequest) string {
	var b strings.Builder
	for _, c := range req.GetUserMessage().GetContent() {
		b.WriteString(c.GetText().GetText())
	}
	return b.String()
}

func isServerRequestFrame(msg tea.Msg) bool {
	fm, ok := msg.(frameMsg)
	return ok && fm.frame.GetServerRequest() != nil
}

// assertAnswer checks one answer: its request id and its approval.
func assertAnswer(t *testing.T, ingest *commonpb.ChannelStreamIngestRequest, requestID string, approved bool) {
	t.Helper()
	if got := ingest.GetClientResponse().GetRequestId(); got != requestID {
		t.Fatalf("answer echoes request_id %q, want %q", got, requestID)
	}
	if got := approvedOf(t, ingest); got != approved {
		t.Fatalf("answer to %s is approved=%v, want %v", requestID, got, approved)
	}
}

// approvedOf is the answer a ClientResponse ingest carries. Its result_json
// must be exactly an object holding a boolean approved.
func approvedOf(t *testing.T, ingest *commonpb.ChannelStreamIngestRequest) bool {
	t.Helper()
	var got map[string]any
	if err := json.Unmarshal([]byte(ingest.GetClientResponse().GetResultJson()), &got); err != nil {
		t.Fatalf("result_json %q is not a JSON object: %v", ingest.GetClientResponse().GetResultJson(), err)
	}
	approved, ok := got["approved"].(bool)
	if !ok || len(got) != 1 {
		t.Fatalf("result_json %q is not {\"approved\":<bool>}", ingest.GetClientResponse().GetResultJson())
	}
	return approved
}
