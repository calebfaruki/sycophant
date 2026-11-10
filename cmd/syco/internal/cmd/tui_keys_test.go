// The tests in this file cover the chat keys and commands.

package cmd

import (
	"fmt"
	"strings"
	"sync"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

const stopping = "stopping…"

// --- Esc ---

func TestTUIEscDuringTurnCancelsItAndShowsStoppingUntilCancelled(t *testing.T) {
	const msgID = "msg-esc-cancel"
	f, d, stream := tuiTurn(t, msgID)

	d.update(keyPress(tea.KeyEscape))
	f.nextTakeBack(t, 5*time.Second)
	req := d.waitCancel()
	if req.GetAgent() != testAgent {
		t.Fatalf("CancelTurn named agent %q, want %q", req.GetAgent(), testAgent)
	}
	d.waitView("the stopping indicator", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, stopping) && !strings.Contains(v, working)
	})

	stream.push(turnStateFrame(msgID, commonpb.TurnState_TURN_STATE_CANCELLED))
	d.waitView("the cancelled turn", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, "Turn cancelled") && !strings.Contains(v, stopping)
	})
}

func TestTUIEscTakesBackAQueuedMessageIntoTheInputWithoutCancelling(t *testing.T) {
	f, d, _ := tuiTurn(t, "msg-esc-takeback")
	d.send("QueuedOcelot")
	f.setTakeBack("QueuedOcelot")
	d.update(keyText("DraftMarmot"))

	d.update(keyPress(tea.KeyEscape))
	req := f.nextTakeBack(t, 5*time.Second)
	if req.GetAgent() != testAgent {
		t.Fatalf("TakeBackMessage named agent %q, want %q", req.GetAgent(), testAgent)
	}
	d.waitView("the taken text in the input", time.Now().Add(5*time.Second), func(string) bool {
		return d.inputValue() == "DraftMarmot QueuedOcelot"
	})
	f.noCancel(t, 300*time.Millisecond)
	if v := d.view(); !strings.Contains(v, working) {
		t.Fatalf("after a take-back, want the turn still running:\n%s", v)
	}
}

// Only the gateway's turn-state frames say a turn is running: the ingest
// ack alone does not. The message waiting with no turn running (one queued
// behind a clear or compact) is not drawn. Esc then still takes it back
// into the input and, with no turn running, cancels nothing. A second Esc,
// with nothing left to take back, neither quits nor cancels.
func TestTUIAckWithNoTurnStateShowsNothingNotWorkingAndEscTakesItBackWithoutCancellingOrQuitting(t *testing.T) {
	f, d, _ := connectedTUI(t)
	d.send("WaitingHeron")
	d.settle(300 * time.Millisecond)
	if v := d.view(); strings.Contains(v, working) {
		t.Fatalf("after the ack with no turn-state frame, want no working indicator:\n%s", v)
	}
	if strings.Contains(d.transcript(), "WaitingHeron") {
		t.Fatalf("a message whose turn has not started must not be drawn:\n%s", d.transcript())
	}

	f.setTakeBack("WaitingHeron")
	d.update(keyPress(tea.KeyEscape))
	if req := f.nextTakeBack(t, 5*time.Second); req.GetAgent() != testAgent {
		t.Fatalf("TakeBackMessage named agent %q, want %q", req.GetAgent(), testAgent)
	}
	d.waitView("the taken text in the input", time.Now().Add(5*time.Second), func(string) bool {
		return d.inputValue() == "WaitingHeron"
	})
	d.settle(300 * time.Millisecond)
	if strings.Contains(d.transcript(), "WaitingHeron") {
		t.Fatalf("a taken-back message must leave no sent message:\n%s", d.transcript())
	}

	d.update(keyPress(tea.KeyEscape))
	f.nextTakeBack(t, 5*time.Second)
	d.settle(300 * time.Millisecond)
	if d.quit {
		t.Fatalf("Esc with nothing to take back and no turn running quit the chat")
	}
	f.noCancel(t, 300*time.Millisecond)
}

// A profile that names an agent opens straight into the chat. Esc before
// the first connect lands does nothing: no RPC, no crash.
func TestTUIEscBeforeTheFirstConnectDoesNothing(t *testing.T) {
	newClientEnv(t)
	cfgDir, p, token := testProfile(t)
	m := newTUIModel(cfgDir, p, token)
	m.Update(tea.WindowSizeMsg{Width: 160, Height: 200})
	_, cmd := m.Update(keyPress(tea.KeyEscape))
	if cmd != nil {
		t.Fatalf("Esc before the first connect returned a command")
	}
}

// After the stream drops mid-turn and while the reconnect is failing, Esc
// sends nothing and shows no error; once the reconnect lands Esc takes a
// message back again.
func TestTUIEscDuringAReconnectAfterADropDoesNothingUntilReconnected(t *testing.T) {
	f, d, s := tuiTurn(t, "msg-esc-drop")
	d.fastRetries()
	f.failReceives(1000, "GatewayDownTern")
	s.end()
	d.waitView("the drop", time.Now().Add(5*time.Second), func(string) bool {
		return strings.HasPrefix(d.footerLeft(), "disconnected")
	})
	d.waitView("a failed reconnect", time.Now().Add(5*time.Second), func(string) bool {
		return f.attempts() >= 3
	})
	d.update(keyPress(tea.KeyEscape))
	d.settle(300 * time.Millisecond)
	f.noTakeBack(t, 200*time.Millisecond)
	f.noCancel(t, 200*time.Millisecond)
	if tr := d.transcript(); strings.Contains(tr, "⚠") {
		t.Fatalf("Esc while disconnected showed an error:\n%s", tr)
	}

	f.failReceives(0, "")
	d.waitConnected()
	f.nextStream(t, 5*time.Second)
	f.setTakeBack("BackAgainAvocet")
	d.update(keyPress(tea.KeyEscape))
	f.nextTakeBack(t, 5*time.Second)
	d.waitView("the taken text in the input", time.Now().Add(5*time.Second), func(string) bool {
		return d.inputValue() == "BackAgainAvocet"
	})
}

// Esc while "stopping…" shows still takes back a message sent after it.
func TestTUIEscWhileStoppingStillTakesBackAMessageSentAfterIt(t *testing.T) {
	f, d, _ := tuiTurn(t, "msg-esc-stopping")
	d.update(keyPress(tea.KeyEscape))
	f.nextTakeBack(t, 5*time.Second)
	d.waitCancel()
	d.waitView("stopping", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, stopping)
	})
	d.send("LateLynx")
	f.setTakeBack("LateLynx")
	d.update(keyPress(tea.KeyEscape))
	f.nextTakeBack(t, 2*time.Second)
	d.waitView("LateLynx back in the input", time.Now().Add(5*time.Second), func(string) bool {
		return d.inputValue() == "LateLynx"
	})
}

func TestTUICancelTurnErrorShowsInTranscriptAndKeepsRunning(t *testing.T) {
	const msgID = "msg-esc-error"
	f, d, _ := tuiTurn(t, msgID)
	f.setCancelErr(status.Error(codes.FailedPrecondition, "CancelRefusedMongoose"))

	d.update(keyPress(tea.KeyEscape))
	d.waitCancel()
	d.waitView("the cancel error with the turn still running", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(d.transcript(), "⚠ cancelling turn: CancelRefusedMongoose") && strings.Contains(v, working) && !strings.Contains(v, stopping)
	})
}

// --- Queue ---

func TestTUIEnterDuringTurnSendsEachMessageAtOnceShowsNothingUntilItsTurnStartsAndUpRecallsTheLast(t *testing.T) {
	const msgID = "msg-queue-show"
	f, d, stream := tuiTurn(t, msgID)

	d.send("SentFirstWombat")
	d.send("SentSecondPangolin")

	log := f.ingestLog()
	if got := []string{ingestText(log[1]), ingestText(log[2])}; got[0] != "SentFirstWombat" || got[1] != "SentSecondPangolin" {
		t.Fatalf("ingests sent while the turn ran carry %q, want both messages in the order typed", got)
	}
	d.settle(300 * time.Millisecond)
	if tr := d.transcript(); strings.Contains(tr, "SentFirstWombat") || strings.Contains(tr, "SentSecondPangolin") {
		t.Fatalf("a message sent mid-turn shows before its turn starts:\n%s", tr)
	}
	d.wantInput("sending while a turn runs", "")
	d.update(keyPress(tea.KeyUp))
	d.wantInput("Up after sending mid-turn", "SentSecondPangolin")
	d.update(keyPress(tea.KeyDown))

	// The first message's turn starts. The log holds the messages up to
	// it, not the one still queued behind it.
	f.setHistory(userEntry("UserTurnLynx"), userEntry("SentFirstWombat"))
	stream.push(turnStateFrame(msgID, commonpb.TurnState_TURN_STATE_IDLE))
	stream.push(turnStateFrame("msg-wombat", commonpb.TurnState_TURN_STATE_WORKING))
	d.waitView("the first mid-turn message sent at its turn start", time.Now().Add(5*time.Second), func(string) bool {
		return d.hasSentPanel("SentFirstWombat")
	})
	d.settle(300 * time.Millisecond)
	if strings.Contains(d.transcript(), "SentSecondPangolin") {
		t.Fatalf("at the first message's turn start, want it sent and the second not shown:\n%s", d.transcript())
	}
}

// A turn-start frame can reach the TUI before its message's ingest ack.
// The message shows nothing from Enter, shows sent at its turn start, and
// the late ack changes nothing.
func TestTUISentMessageShowsOnlyAtItsTurnStartEvenBeforeItsAck(t *testing.T) {
	const msgID = "msg-early-otter"
	f, d, stream := connectedTUI(t)
	release := f.holdAcks()
	d.send("EarlyOtter")
	d.settle(300 * time.Millisecond)
	if strings.Contains(d.transcript(), "EarlyOtter") {
		t.Fatalf("a message whose turn has not started must not be drawn:\n%s", d.transcript())
	}

	stream.push(turnStateFrame(msgID, commonpb.TurnState_TURN_STATE_WORKING))
	d.waitView("the message sent at its turn start", time.Now().Add(5*time.Second), func(string) bool {
		return d.hasSentPanel("EarlyOtter")
	})
	release()
	d.settle(300 * time.Millisecond)
	if n := strings.Count(d.transcript(), "EarlyOtter"); n != 1 || !d.hasSentPanel("EarlyOtter") {
		t.Fatalf("after its turn started and its ack landed, want the message sent once (shown %d times):\n%s", n, d.transcript())
	}
}

// A message the relay refuses shows the reason and goes back into the
// input. The stream is fine, so the TUI stays connected.
func TestTUIRefusedMessageShowsTheReasonAndReturnsToTheInputStillConnected(t *testing.T) {
	f, d, _ := connectedTUI(t)
	f.failIngests(status.Error(codes.PermissionDenied, "IngestRefusedKite"))
	d.send("RefusedMessageKite")
	d.waitView("the refusal and the message back in the input", time.Now().Add(5*time.Second), func(string) bool {
		return strings.Contains(d.transcript(), "⚠ sending message: IngestRefusedKite") && d.inputValue() == "RefusedMessageKite"
	})
	d.settle(300 * time.Millisecond)
	if left := d.footerLeft(); !strings.HasPrefix(left, "tester · ") {
		t.Fatalf("after a refused message the line under the panel is %q, want still connected", left)
	}
	if n := f.attempts(); n != 1 {
		t.Fatalf("fake saw %d connect attempts, want the first one only", n)
	}
}

// A reload answered after a later one started changes nothing.
func TestTUISupersededReloadChangesNothing(t *testing.T) {
	_, d, _ := tuiTurn(t, "msg-stale-reload")
	before := d.transcript()
	m := d.model.(*tuiModel)
	d.update(historyMsg{gen: m.reloadGen - 1, entries: []*commonpb.HistoryEntry{userEntry("UserStaleReloadMarten")}})
	if got := d.transcript(); got != before {
		t.Fatalf("a superseded reload changed the transcript:\nbefore:\n%s\nafter:\n%s", before, got)
	}
}

// A turn start's reload answered after a reconnect landed changes nothing.
func TestTUIReloadSupersededByAReconnectChangesNothing(t *testing.T) {
	_, d, s := tuiTurn(t, "msg-stale-reconnect")
	gen := d.model.(*tuiModel).reloadGen
	d.dropAndReconnect(s)
	before := d.transcript()
	d.update(historyMsg{gen: gen, entries: []*commonpb.HistoryEntry{userEntry("UserStaleReloadMarten")}})
	if got := d.transcript(); got != before {
		t.Fatalf("a reload superseded by a reconnect changed the transcript:\nbefore:\n%s\nafter:\n%s", before, got)
	}
}

// A turn start's reload answered after a /clear ran changes nothing.
func TestTUIReloadSupersededByAClearChangesNothing(t *testing.T) {
	f, d, _ := tuiTurn(t, "msg-stale-clear")
	gen := d.model.(*tuiModel).reloadGen
	d.typeEnter("/clear")
	f.nextContext(t, 5*time.Second)
	d.waitView("the clear notice", time.Now().Add(5*time.Second), func(string) bool {
		return strings.Contains(d.transcript(), "context cleared")
	})
	before := d.transcript()
	d.update(historyMsg{gen: gen, entries: []*commonpb.HistoryEntry{userEntry("UserStaleReloadMarten")}})
	if got := d.transcript(); got != before {
		t.Fatalf("a reload superseded by a clear changed the transcript:\nbefore:\n%s\nafter:\n%s", before, got)
	}
}

// Frames that arrive while a turn start's reload is on its way stay in the
// transcript after it.
func TestTUIFramesBeforeTheTurnStartReloadStayAfterIt(t *testing.T) {
	const msgID = "msg-live-reload"
	f, d, stream := connectedTUI(t)
	d.send("UserLiveReloadLynx")
	release := sync.OnceFunc(f.holdHistory())
	defer release()
	stream.push(turnStateFrame(msgID, commonpb.TurnState_TURN_STATE_WORKING))
	stream.push(toolStartFrame(msgID, "item-live", "LiveToolShrike"))
	d.waitView("the live tool row", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, "LiveToolShrike")
	})
	release()
	d.waitView("the reload", time.Now().Add(5*time.Second), func(string) bool {
		return d.hasSentPanel("UserLiveReloadLynx")
	})
	lines := d.transcriptLines()
	if tool := rowIndex(lines, "LiveToolShrike"); tool <= rowIndex(lines, "UserLiveReloadLynx") {
		t.Fatalf("the reload dropped the tool row that arrived before it, or moved it above its message:\n%s", d.transcript())
	}
}

func TestTUIInputStaysBlockedWhileApprovalShows(t *testing.T) {
	_, d, stream := tuiTurn(t, "msg-queue-approval")
	stream.push(approvalRequest(t, "req-block"))
	d.waitMsg("the approval request frame", 5*time.Second, isServerRequestFrame)

	d.update(keyText("x"))
	d.wantInput("typing while an approval shows", "")
	d.update(keyText("y"))
	d.wantInput("the y answer", "")
}

// --- Context commands ---

func TestTUIClearAndCompactCallTheirRPCsAndShowARefusal(t *testing.T) {
	// An empty conversation: the result shows even from the start screen.
	f, d, _ := connectedTUI(t)

	d.typeEnter("/clear")
	if kind := f.nextContext(t, 5*time.Second); kind != "clear" {
		t.Fatalf("/clear sent %q, want clear", kind)
	}
	d.waitView("the clear notice", time.Now().Add(5*time.Second), func(string) bool {
		return strings.Contains(d.transcript(), "context cleared")
	})

	f.setContextErr(status.Error(codes.FailedPrecondition, "CompactRefusedOcelot"))
	d.typeEnter("/compact")
	if kind := f.nextContext(t, 5*time.Second); kind != "compact" {
		t.Fatalf("/compact sent %q, want compact", kind)
	}
	d.waitView("the compact refusal", time.Now().Add(5*time.Second), func(string) bool {
		return strings.Contains(d.transcript(), "⚠ /compact: CompactRefusedOcelot")
	})

	if n := len(f.ingestLog()); n != 0 {
		t.Fatalf("/clear and /compact sent %d chat messages, want none", n)
	}
}

// beforeClear opens the TUI on a conversation holding one message.
func beforeClear(t *testing.T) (*fakeGateway, *tuiDriver) {
	t.Helper()
	f, _ := newClientEnv(t)
	f.setHistory(userEntry("UserBeforeClearPuffin"))
	d := openTUI(t, f)
	f.nextStream(t, 5*time.Second)
	d.waitConnected()
	d.waitView("the conversation", time.Now().Add(5*time.Second), func(string) bool {
		return d.hasSentPanel("UserBeforeClearPuffin")
	})
	return f, d
}

func TestTUIClearLeavesOnlyTheConversationTheRelayReturns(t *testing.T) {
	f, d := beforeClear(t)
	d.typeEnter("/clear")
	f.nextContext(t, 5*time.Second)
	d.waitView("the clear notice", time.Now().Add(5*time.Second), func(string) bool {
		return strings.Contains(d.transcript(), "context cleared")
	})
	if tr := d.transcript(); strings.Contains(tr, "UserBeforeClearPuffin") {
		t.Fatalf("after /clear the old conversation still shows:\n%s", tr)
	}
}

func TestTUICompactRedrawsTheConversation(t *testing.T) {
	f, d := beforeClear(t)
	d.typeEnter("/compact")
	f.nextContext(t, 5*time.Second)
	d.waitView("the compact notice", time.Now().Add(5*time.Second), func(string) bool {
		return strings.Contains(d.transcript(), "context compacted")
	})
	if !d.hasSentPanel("UserBeforeClearPuffin") {
		t.Fatalf("after /compact the conversation is not drawn:\n%s", d.transcript())
	}
}

// A clear that ran shows so even when reading the conversation after it
// fails.
func TestTUIClearThatRanShowsItsNoticeThenTheReloadError(t *testing.T) {
	f, d := beforeClear(t)
	f.failHistory(status.Error(codes.Unavailable, "HistoryDownSkua"))
	d.typeEnter("/clear")
	f.nextContext(t, 5*time.Second)
	d.waitView("the clear notice and the reload error", time.Now().Add(5*time.Second), func(string) bool {
		return strings.Contains(d.transcript(), "HistoryDownSkua")
	})
	tr := d.transcript()
	cleared, failed := strings.Index(tr, "context cleared"), strings.Index(tr, "⚠ loading the conversation of "+testAgent+": HistoryDownSkua")
	if cleared < 0 || cleared > failed {
		t.Fatalf("want the clear notice, then the reload error:\n%s", tr)
	}
	if strings.Contains(tr, "UserBeforeClearPuffin") {
		t.Fatalf("after /clear the old conversation still shows:\n%s", tr)
	}
}

// Reply text still streaming when a /clear lands does not come back when
// its turn ends.
func TestTUIClearDropsTheReplyStillStreaming(t *testing.T) {
	const msgID = "msg-clear-streaming"
	f, d, s := tuiTurn(t, msgID)
	s.push(streamTextFrames(msgID, "item-1", "StreamingReplyGrebe")...)
	d.waitView("the streaming reply", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, "StreamingReplyGrebe")
	})
	d.typeEnter("/clear")
	f.nextContext(t, 5*time.Second)
	d.waitView("the clear notice", time.Now().Add(5*time.Second), func(string) bool {
		return strings.Contains(d.transcript(), "context cleared")
	})
	// Hold the reload at the turn's end: it redraws the cleared
	// conversation and would hide the reply.
	release := f.holdHistory()
	defer release()
	d.finishTurn(s, msgID)
	if tr := d.transcript(); strings.Contains(tr, "StreamingReplyGrebe") {
		t.Fatalf("a reply cleared while it streamed came back at its turn's end:\n%s", tr)
	}
}

// --- Scrolling ---

// tallTranscript opens the TUI on an agent whose conversation is taller
// than a 12-line screen.
func tallTranscript(t *testing.T) *tuiDriver {
	t.Helper()
	f, _ := newClientEnv(t)
	var entries []*commonpb.HistoryEntry
	for i := range 40 {
		entries = append(entries, userEntry(fmt.Sprintf("ScrollLineHeron%02d", i)))
	}
	f.setHistory(entries...)
	d := openTUI(t, f)
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
