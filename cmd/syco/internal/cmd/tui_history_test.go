// The tests in this file cover recalling sent messages.

package cmd

import (
	"os"
	"path/filepath"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/sycophant-md/sycophant/cmd/syco/internal/config"
	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

// enterIdle presses Enter and waits for the turn it starts to end.
func (d *tuiDriver) enterIdle(f *fakeGateway, stream *fakeStream, conv string) {
	d.t.Helper()
	m := d.model.(*tuiModel)
	n := len(f.ingestLog()) + 1
	d.update(keyPress(tea.KeyEnter))
	d.waitIngests(f, n)
	d.waitView("the ingest ack", time.Now().Add(5*time.Second), func(string) bool { return !m.sending })
	d.finishTurn(stream, conv)
}

// sendIdle sends text on a connected TUI and waits for its turn to end.
func (d *tuiDriver) sendIdle(f *fakeGateway, stream *fakeStream, conv, text string) {
	d.t.Helper()
	d.update(keyText(text))
	d.enterIdle(f, stream, conv)
}

func TestTUIUpAndDownBrowseSentMessagesAndKeepTheDraft(t *testing.T) {
	const conv = "conv-recall"
	f, d, stream := connectedTUI(t, conv)
	d.sendIdle(f, stream, conv, "OldestCrane")
	d.sendIdle(f, stream, conv, "MiddleEgret")
	d.sendIdle(f, stream, conv, "NewestStork")

	d.update(keyText("DraftPelican"))
	d.update(keyPress(tea.KeyUp))
	d.wantInput("one Up", "NewestStork")
	d.update(keyPress(tea.KeyUp))
	d.update(keyPress(tea.KeyUp))
	d.wantInput("three Ups", "OldestCrane")
	d.update(keyPress(tea.KeyUp))
	d.wantInput("Up past the oldest", "OldestCrane")

	d.update(keyPress(tea.KeyDown))
	d.wantInput("Down", "MiddleEgret")
	d.update(keyPress(tea.KeyDown))
	d.update(keyPress(tea.KeyDown))
	d.wantInput("Down past the newest", "DraftPelican")
	d.update(keyPress(tea.KeyDown))
	d.wantInput("another Down", "DraftPelican")
}

func TestTUIUpAndDownMoveBetweenInputLinesAndBrowseOnlyFromTheEnds(t *testing.T) {
	const conv = "conv-recall-lines"
	f, d, stream := connectedTUI(t, conv)
	d.sendIdle(f, stream, conv, "EarlierWren")

	d.typeLines("DraftTopFinch", "DraftBottomRobin")
	d.update(keyPress(tea.KeyUp))
	d.wantInput("Up from the bottom line", "DraftTopFinch\nDraftBottomRobin")
	d.update(keyPress(tea.KeyUp))
	d.wantInput("Up from the top line", "EarlierWren")

	d.update(keyPress(tea.KeyDown))
	d.wantInput("Down past the newest", "DraftTopFinch\nDraftBottomRobin")

	// A recalled multi-line message: Down from its top line moves the
	// cursor; Down from its bottom line browses forward.
	d.enterIdle(f, stream, conv) // Enter sends the restored draft.
	d.update(keyPress(tea.KeyUp))
	d.wantInput("Up on an empty input", "DraftTopFinch\nDraftBottomRobin")
	d.update(keyPress(tea.KeyDown))
	d.wantInput("Down from the recalled message's top line", "DraftTopFinch\nDraftBottomRobin")
	d.update(keyPress(tea.KeyDown))
	d.wantInput("Down from its bottom line", "")
}

func TestTUISentMessagesAreRecalledAfterARestartWithTheirLineBreaks(t *testing.T) {
	const conv = "conv-recall-restart"
	f, d, stream := connectedTUI(t, conv)
	d.typeLines("RestartLineOneBadger", "RestartLineTwoMole")
	d.enterIdle(f, stream, conv)

	f.setFreshID("conv-recall-restart-2")
	d2 := openTUI(t, false, "")
	f.nextStream(t, 5*time.Second)
	d2.waitConnected()
	d2.update(keyPress(tea.KeyUp))
	d2.wantInput("Up in a new TUI", "RestartLineOneBadger\nRestartLineTwoMole")
}

func TestTUIQueuedMessagesJoinTheHistoryWhenSent(t *testing.T) {
	const conv = "conv-recall-queue"
	f, d, stream := tuiTurn(t, conv)

	d.update(keyText("QueuedHistoryVole"))
	d.update(keyPress(tea.KeyEnter))
	if got := config.LoadInputHistory(config.ConfigDir()); len(got) != 1 {
		t.Fatalf("history is %q while the message is queued, want only the sent one", got)
	}
	stream.push(turnStateFrame(conv, commonpb.TurnState_TURN_STATE_IDLE))
	d.waitIngests(f, 2)
	got := config.LoadInputHistory(config.ConfigDir())
	if len(got) != 2 || got[1] != "QueuedHistoryVole" {
		t.Fatalf("history is %q after the queued message is sent, want it last", got)
	}
}

func TestTUIOpensAndSendsWhenTheHistoryFileIsUnreadable(t *testing.T) {
	const conv = "conv-recall-unreadable"
	f, dir := newClientEnv(t)
	if err := os.Mkdir(filepath.Join(dir, config.InputHistoryFile), 0o755); err != nil {
		t.Fatal(err)
	}
	f.setFreshID(conv)
	d := openTUI(t, false, "")
	f.nextStream(t, 5*time.Second)
	d.waitConnected()
	d.send("UnreadableHistoryShrew")
	d.update(keyPress(tea.KeyUp))
	d.wantInput("Up after sending", "UnreadableHistoryShrew")
}
