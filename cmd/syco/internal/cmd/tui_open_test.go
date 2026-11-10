// The tests in this file open the TUI on the agent's one conversation.

package cmd

import (
	"strings"
	"testing"
	"time"

	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

func TestTUIOpenOnRunningTurnShowsWorking(t *testing.T) {
	f, _ := newClientEnv(t)
	f.setHistory(userEntry("UserPendingHeron"))
	f.setTurnState(commonpb.TurnState_TURN_STATE_WORKING)

	d := openTUI(t, f)
	d.waitView("the working indicator", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, working)
	})
}

func TestTUIAfterDropRedrawsFromHistoryWithoutPreDropText(t *testing.T) {
	f, _ := newClientEnv(t)
	const (
		msgID   = "msg-tui-drop"
		preDrop = "StreamPredropMarmot"
		final   = "AssistantFinalNarwhal"
	)
	f.setHistory(userEntry("UserEarlierPelican"), assistantEntry("AssistantEarlierQuokka"))

	d := openTUI(t, f)
	first := f.nextStream(t, 5*time.Second)
	d.send("UserNewMessageOtter")
	first.push(turnStateFrame(msgID, commonpb.TurnState_TURN_STATE_WORKING))
	first.push(streamTextFrames(msgID, "item-predrop", preDrop)...)
	d.waitView("the pre-drop stream text", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, preDrop)
	})

	// The turn finishes while the stream is down.
	f.appendHistory(assistantEntry(final))
	f.setTurnState(commonpb.TurnState_TURN_STATE_IDLE)
	first.end()
	d.waitView("the reply from history with no pre-drop text",
		time.Now().Add(10*time.Second), func(v string) bool {
			return strings.Contains(v, final) && !strings.Contains(v, preDrop)
		})
}

// A turn that ends while the TUI connects shows its reply once: the
// connect's history read already holds it, and the stream still carries it.
func TestTUITurnEndingWhileConnectingShowsItsReplyOnce(t *testing.T) {
	const msgID, reply = "msg-connect-window", "ReplyConnectWindowGrebe"
	f, _ := newClientEnv(t)
	f.setHistory(userEntry("UserConnectWindowHeron"))
	release := f.holdHistory()
	d := openTUI(t, f)
	s := f.nextStream(t, 5*time.Second)
	s.push(streamTextFrames(msgID, "item-1", reply)...)
	s.push(turnStateFrame(msgID, commonpb.TurnState_TURN_STATE_IDLE))
	f.appendHistory(assistantEntry(reply))
	release()
	d.waitConnected()
	d.settle(500 * time.Millisecond)
	if n := strings.Count(d.transcript(), reply); n != 1 {
		t.Fatalf("the reply shows %d times, want once:\n%s", n, d.transcript())
	}
}
