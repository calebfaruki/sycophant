// The tests in this file cover the chat keys and the message queue.

package cmd

import (
	"errors"
	"fmt"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

const stopping = "stopping…"

// --- Esc ---

func TestTUIEscDuringTurnCancelsItAndShowsStoppingUntilCancelled(t *testing.T) {
	const conv = "conv-esc-cancel"
	f, d, stream := tuiTurn(t, conv)

	d.update(keyPress(tea.KeyEscape))
	req := f.nextCancel(t, 5*time.Second)
	if req.GetConversationId() != conv || req.GetWorkspace() != testWorkspace {
		t.Fatalf("CancelTurn named conversation %q in workspace %q, want %q in %q",
			req.GetConversationId(), req.GetWorkspace(), conv, testWorkspace)
	}
	d.waitView("the stopping indicator", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, stopping) && !strings.Contains(v, working)
	})

	stream.push(turnStateFrame(conv, commonpb.TurnState_TURN_STATE_CANCELLED))
	d.waitView("the cancelled turn", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, "Turn cancelled") && !strings.Contains(v, stopping)
	})
}

func TestTUIEscWithQueuedMessagesTakesThemBackWithoutCancelling(t *testing.T) {
	const conv = "conv-esc-queue"
	f, d, _ := tuiTurn(t, conv)
	d.typeEnter("QueuedFirstOcelot")
	d.typeEnter("QueuedSecondTapir")
	d.update(keyText("DraftMarmot"))

	d.update(keyPress(tea.KeyEscape))
	f.noCancel(t, 300*time.Millisecond)
	const want = "DraftMarmot QueuedFirstOcelot QueuedSecondTapir"
	d.wantInput("Esc with a queue", want)
	if v := d.view(); strings.Contains(v, "queued:") || !strings.Contains(v, working) {
		t.Fatalf("after Esc with a queue, want no queued lines and the turn still running:\n%s", v)
	}
}

func TestTUIEscWithNoTurnRunningNeitherQuitsNorCancels(t *testing.T) {
	f, d, _ := connectedTUI(t, "conv-esc-idle")

	d.update(keyPress(tea.KeyEscape))
	d.settle(300 * time.Millisecond)
	if v := d.view(); !strings.Contains(v, placeholder) {
		t.Fatalf("view after Esc with no turn running is %q; the TUI must keep running", v)
	}
	f.noCancel(t, 100*time.Millisecond)
}

func TestTUICancelTurnErrorShowsInTranscriptAndKeepsRunning(t *testing.T) {
	const conv = "conv-esc-error"
	f, d, _ := tuiTurn(t, conv)
	f.setCancelErr(errors.New("CancelRefusedMongoose"))

	d.update(keyPress(tea.KeyEscape))
	f.nextCancel(t, 5*time.Second)
	d.waitView("the cancel error with the turn still running", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(d.transcript(), "CancelRefusedMongoose") && strings.Contains(v, working) && !strings.Contains(v, stopping)
	})
}

// --- Queue ---

func TestTUIEnterDuringTurnQueuesMessagesAbovePanelInsteadOfSending(t *testing.T) {
	const conv = "conv-queue-show"
	f, d, _ := tuiTurn(t, conv)

	d.typeEnter("QueuedFirstWombat")
	d.typeEnter("QueuedSecondPangolin")
	d.update(keyPress(tea.KeyEnter)) // empty input: nothing
	d.settle(300 * time.Millisecond)

	if n := len(f.ingestLog()); n != 1 {
		t.Fatalf("fake saw %d ingests while the turn ran, want only the first message", n)
	}
	v := d.view()
	if d.hasSentPanel("QueuedFirstWombat") {
		t.Fatalf("queued message shows as sent in the transcript:\n%s", v)
	}
	lines := strings.Split(v, "\n")
	panel := panelTop(t, lines)
	if !strings.Contains(lines[panel-2], "queued: QueuedFirstWombat") ||
		!strings.Contains(lines[panel-1], "queued: QueuedSecondPangolin") {
		t.Fatalf("queued lines above the panel are %q and %q, want the two messages oldest first",
			lines[panel-2], lines[panel-1])
	}
	if n := strings.Count(v, "queued:"); n != 2 {
		t.Fatalf("view shows %d queued lines, want 2; an empty Enter queued one:\n%s", n, v)
	}
	d.wantInput("queuing", "")
}

func TestTUIQueuedLineIsTruncatedToScreenWidth(t *testing.T) {
	_, d, _ := tuiTurn(t, "conv-queue-width")
	d.update(tea.WindowSizeMsg{Width: 40, Height: 30})

	d.typeEnter("QueuedLongYak " + strings.Repeat("x", 100))
	for _, l := range strings.Split(d.view(), "\n") {
		if strings.Contains(l, "queued:") && (ansi.StringWidth(l) > 40 || !strings.HasSuffix(l, "…")) {
			t.Fatalf("queued line %q is %d wide, want at most 40 and cut with …", l, ansi.StringWidth(l))
		}
	}
	if !strings.Contains(d.view(), "queued: QueuedLongYak") {
		t.Fatalf("no queued line in view:\n%s", d.view())
	}
}

func TestTUITurnEndingIdleSendsOldestQueuedMessageAsNextTurn(t *testing.T) {
	const conv = "conv-queue-idle"
	f, d, stream := tuiTurn(t, conv)
	d.typeEnter("QueuedFirstWombat")
	d.typeEnter("QueuedSecondPangolin")

	stream.push(turnStateFrame(conv, commonpb.TurnState_TURN_STATE_IDLE))
	d.waitIngests(f, 2)
	second := f.ingestLog()[1]
	if got := ingestText(second); got != "QueuedFirstWombat" {
		t.Fatalf("second ingest carries %q, want the oldest queued message", got)
	}
	if second.GetConversationId() != conv {
		t.Fatalf("second ingest names conversation %q, want %q", second.GetConversationId(), conv)
	}
	d.waitView("the sent message in the transcript and the other still queued", time.Now().Add(5*time.Second), func(v string) bool {
		return d.hasSentPanel("QueuedFirstWombat") && !strings.Contains(v, "queued: QueuedFirstWombat") &&
			strings.Contains(v, "queued: QueuedSecondPangolin") && strings.Contains(v, working)
	})
}

func TestTUITurnEndingCancelledPutsQueuedTextBackInInput(t *testing.T) {
	const conv = "conv-queue-return"
	_, d, stream := tuiTurn(t, conv)
	d.typeEnter("QueuedFirstWombat")
	d.typeEnter("QueuedSecondPangolin")
	d.update(keyText("DraftCapybara"))

	stream.push(turnStateFrame(conv, commonpb.TurnState_TURN_STATE_CANCELLED))
	d.waitView("the turn to end", time.Now().Add(5*time.Second), func(v string) bool {
		return !strings.Contains(v, working)
	})
	d.settle(200 * time.Millisecond)

	d.wantInput("the turn ended", "DraftCapybara QueuedFirstWombat QueuedSecondPangolin")
	if v := d.view(); strings.Contains(v, "queued:") {
		t.Fatalf("queued lines remain after the turn ended:\n%s", v)
	}
}

func TestTUIInputStaysBlockedWhileApprovalShows(t *testing.T) {
	_, d, stream := tuiTurn(t, "conv-queue-approval")
	stream.push(approvalRequest(t, "req-block"))
	d.waitMsg("the approval request frame", 5*time.Second, isServerRequestFrame)

	d.update(keyText("x"))
	d.wantInput("typing while an approval shows", "")
	d.update(keyText("y"))
	d.wantInput("the y answer", "")
}

// --- Scrolling ---

// tallTranscript opens the TUI on a conversation whose history is taller
// than a 12-line screen.
func tallTranscript(t *testing.T) *tuiDriver {
	t.Helper()
	f, dir := newClientEnv(t)
	const conv = "conv-tall"
	var entries []*commonpb.HistoryEntry
	for i := range 40 {
		entries = append(entries, userEntry(fmt.Sprintf("ScrollLineHeron%02d", i)))
	}
	f.setHistory(conv, entries...)
	saveHistoryLine(t, dir, conv)
	d := openTUI(t, true, "")
	d.update(tea.WindowSizeMsg{Width: 80, Height: 12})
	d.waitView("the end of the transcript", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, "ScrollLineHeron39")
	})
	return d
}

func TestTUITypingDoesNotScrollTheTranscript(t *testing.T) {
	d := tallTranscript(t)
	// Off the bottom, so a key that scrolled either way would move it.
	d.update(keyPress(tea.KeyPgUp))
	before := d.transcript()

	keys := []tea.KeyPressMsg{
		keyText("k"),
		keyText("b"),
		keyText("u"),
		keyText(" "),
		keyText("j"),
		keyPress(tea.KeyUp),
		keyPress(tea.KeyDown),
	}
	for _, k := range keys {
		d.update(k)
		if got := d.transcript(); got != before {
			t.Fatalf("key %q moved the transcript:\nbefore:\n%s\nafter:\n%s", k.String(), before, got)
		}
	}
	d.wantInput("typing", "kbu j")
}

func TestTUIPgUpPgDnAndMouseWheelScrollTheTranscript(t *testing.T) {
	d := tallTranscript(t)
	bottom := d.transcript()

	d.update(keyPress(tea.KeyPgUp))
	if d.transcript() == bottom {
		t.Fatal("PgUp did not scroll the transcript")
	}
	d.update(keyPress(tea.KeyPgDown))
	if got := d.transcript(); got != bottom {
		t.Fatalf("PgDn did not scroll back to the bottom:\n%s", got)
	}
	d.update(tea.MouseWheelMsg{Button: tea.MouseWheelUp})
	if d.transcript() == bottom {
		t.Fatal("the mouse wheel did not scroll the transcript")
	}
	d.wantInput("scrolling keys", "")
}
