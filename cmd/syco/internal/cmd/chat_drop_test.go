package cmd

import (
	"errors"
	"strings"
	"testing"
	"time"

	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

// A one-shot whose stream drops before its turn ends stops and points the
// person at `syco chat`. The conversation is shared, so it neither reads the
// agent-wide turn state nor prints a reply from history: either could be
// someone else's.
func TestOneShotStreamDropExitsAndPointsAtSycoChat(t *testing.T) {
	const (
		msgID      = "msg-drop"
		preDrop    = "StreamPredropMarmot"
		otherReply = "AssistantOtherPersonNarwhal"
	)
	f, _ := newClientEnv(t)
	f.setMessageID(msgID)

	run := startChat("UserNewMessageOtter")
	first := f.nextStream(t, 5*time.Second)
	f.nextIngest(t, 5*time.Second)
	first.push(turnStateFrame(msgID, commonpb.TurnState_TURN_STATE_WORKING))
	first.push(streamTextFrames(msgID, "item-predrop", preDrop)...)

	// Another person's turn ends, failed, while this command's stream is
	// down.
	f.appendHistory(userEntry("UserOtherPersonHeron"), assistantEntry(otherReply))
	f.setTurnState(commonpb.TurnState_TURN_STATE_FAILED)
	first.end()

	err := run.wait(t, 10*time.Second)
	if !errors.Is(err, errConnectionLost) {
		t.Fatalf("one-shot after a drop returned %v, want errConnectionLost", err)
	}
	if !strings.Contains(err.Error(), "syco chat") {
		t.Errorf("the error does not point at syco chat: %v", err)
	}
	out := run.stdout.String()
	for _, text := range []string{preDrop, otherReply} {
		if strings.Contains(out, text) {
			t.Errorf("stdout holds %q after the drop: %q", text, out)
		}
	}
}
