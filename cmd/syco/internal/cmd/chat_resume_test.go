package cmd

import (
	"strings"
	"testing"
	"time"

	"github.com/sycophant-md/sycophant/cmd/syco/internal/config"
	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

// unknownID is well formed, and no fake in these tests knows it.
const unknownID = "3f6c1d2e-7a8b-4c9d-9e0f-1a2b3c4d5e6f"

// refusalBound caps how long a refusal may take. The spec sets no bound; a
// refusal happens before any turn, so this only keeps a run that does not
// refuse from stalling the package.
const refusalBound = 15 * time.Second

func TestTUIContinueFromLeftoverConfigFailsNamingTheUnknownID(t *testing.T) {
	_, dir := newClientEnv(t)
	makeLeftoverConfig(t, dir, unknownID)

	err := startChat("--continue").wait(t, refusalBound)
	if err == nil {
		t.Fatal("TUI --continue on an unknown conversation returned no error")
	}
	if !strings.Contains(err.Error(), unknownID) {
		t.Fatalf("TUI --continue error does not name the unknown ID %s: %v", unknownID, err)
	}
}

func TestChatResumeOfUnknownIDFailsNamingTheIDInOneShotAndTUI(t *testing.T) {
	for _, tc := range []struct {
		name string
		args []string
	}{
		{"one-shot", []string{"--resume", unknownID, "hello"}},
		{"TUI", []string{"--resume", unknownID}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			newClientEnv(t)

			err := startChat(tc.args...).wait(t, refusalBound)
			if err == nil {
				t.Fatalf("--resume of an unknown conversation returned no error")
			}
			if !strings.Contains(err.Error(), unknownID) {
				t.Fatalf("--resume error does not name the unknown ID %s: %v", unknownID, err)
			}
		})
	}
}

func TestOneShotSavesNewConversationIDAtAckBeforeTurnEnds(t *testing.T) {
	f, dir := newClientEnv(t)
	const conv = "conv-new-oneshot"
	f.setFreshID(conv)

	startChat("hello")
	f.nextIngest(t, 5*time.Second)

	deadline := time.Now().Add(5 * time.Second)
	for {
		if e, err := config.HistoryLastForAgent(dir, testAgent); err == nil && e.ConversationID == conv {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("history holds no line for %s in %s after the ack while the turn is still running", conv, testAgent)
		}
		time.Sleep(50 * time.Millisecond)
	}
}

// droppedOneShot opens conversation conv with --continue, sends a message,
// and gets the fake to the point just before the first stream drops: the
// fake has acked the ingest and sent WORKING and preDrop stream text.
func droppedOneShot(t *testing.T, conv, earlierAssistant, preDrop string) (*fakeGateway, *chatRun, *fakeStream) {
	t.Helper()
	f, dir := newClientEnv(t)
	f.setHistory(conv, userEntry("UserEarlierPelican"), assistantEntry(earlierAssistant))
	saveHistoryLine(t, dir, conv)

	run := startChat("--continue", "UserNewMessageOtter")
	first := f.nextStream(t, 5*time.Second)
	f.nextIngest(t, 5*time.Second)
	first.push(turnStateFrame(conv, commonpb.TurnState_TURN_STATE_WORKING))
	first.push(streamTextFrames(conv, "item-predrop", preDrop)...)
	return f, run, first
}

func TestOneShotAfterDropOnFinishedTurnPrintsOnlyTheReplyFromHistory(t *testing.T) {
	const (
		conv             = "conv-drop-finished"
		earlierAssistant = "AssistantEarlierQuokka"
		preDrop          = "StreamPredropMarmot"
		final            = "AssistantFinalNarwhal"
	)
	f, run, first := droppedOneShot(t, conv, earlierAssistant, preDrop)

	// The turn finishes while the stream is down.
	f.appendHistory(conv, assistantEntry(final))
	f.setTurnState(commonpb.TurnState_TURN_STATE_IDLE)
	first.end()
	err := run.wait(t, 10*time.Second)
	if err != nil {
		t.Fatalf("one-shot exited with error after the drop: %v", err)
	}
	out := run.stdout.String()
	if !strings.Contains(out, final) {
		t.Errorf("stdout lacks the reply from history %q: %q", final, out)
	}
	if strings.Contains(out, preDrop) {
		t.Errorf("stdout holds text streamed before the drop %q: %q", preDrop, out)
	}
	if strings.Contains(out, earlierAssistant) {
		t.Errorf("stdout holds the earlier assistant entry %q: %q", earlierAssistant, out)
	}
}

func TestOneShotAfterDropOnRunningTurnPrintsOnlyTheReplyFromHistoryAtIdle(t *testing.T) {
	const (
		conv             = "conv-drop-running"
		earlierAssistant = "AssistantEarlierQuokka"
		preDrop          = "StreamPredropMarmot"
		postDrop         = "StreamPostdropWalrus"
		final            = "AssistantFinalNarwhal"
	)
	f, run, first := droppedOneShot(t, conv, earlierAssistant, preDrop)

	// The turn is still running when the client comes back.
	f.setTurnState(commonpb.TurnState_TURN_STATE_WORKING)
	first.end()

	second := f.nextStream(t, 10*time.Second)
	second.push(streamTextFrames(conv, "item-postdrop", postDrop)...)
	f.appendHistory(conv, assistantEntry(final))
	second.push(turnStateFrame(conv, commonpb.TurnState_TURN_STATE_IDLE))

	// No bound in the spec; this only keeps a hang from stalling the package.
	err := run.wait(t, 60*time.Second)
	if err != nil {
		t.Fatalf("one-shot exited with error: %v", err)
	}
	out := run.stdout.String()
	if !strings.Contains(out, final) {
		t.Errorf("stdout lacks the reply from history %q: %q", final, out)
	}
	for _, streamed := range []string{preDrop, postDrop} {
		if strings.Contains(out, streamed) {
			t.Errorf("stdout holds streamed text %q: %q", streamed, out)
		}
	}
	if strings.Contains(out, earlierAssistant) {
		t.Errorf("stdout holds the earlier assistant entry %q: %q", earlierAssistant, out)
	}
}
