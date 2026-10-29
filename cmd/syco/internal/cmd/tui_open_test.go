// The tests in this file enter the TUI through openTUIModel, the function
// that builds the TUI model for a profile and flags and opens a named
// conversation before the full-screen program starts.

package cmd

import (
	"strings"
	"testing"
	"time"

	"github.com/sycophant-md/sycophant/cmd/syco/internal/config"
	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

func TestTUISavesNewConversationIDAtAckBeforeTurnEnds(t *testing.T) {
	f, dir := newClientEnv(t)
	const conv = "conv-new-tui"
	f.setFreshID(conv)

	d := openTUI(t, false, "")
	d.send("UserNewMessageOtter")

	if e, err := config.HistoryLastForWorkspace(dir, testWorkspace); err != nil || e.ConversationID != conv {
		t.Fatalf("history holds no line for %s in %s after the ack while the turn is still running", conv, testWorkspace)
	}
}

func TestTUIOpenOnRunningTurnShowsWorking(t *testing.T) {
	f, dir := newClientEnv(t)
	const conv = "conv-open-working"
	f.setHistory(conv, userEntry("UserPendingHeron"))
	f.setTurnState(commonpb.TurnState_TURN_STATE_WORKING)
	saveHistoryLine(t, dir, conv)

	d := openTUI(t, true, "")
	d.waitView("the working indicator", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, working)
	})
}

func TestTUIAfterDropRedrawsFromHistoryWithoutPreDropText(t *testing.T) {
	f, dir := newClientEnv(t)
	const (
		conv    = "conv-tui-drop"
		preDrop = "StreamPredropMarmot"
		final   = "AssistantFinalNarwhal"
	)
	f.setHistory(conv, userEntry("UserEarlierPelican"), assistantEntry("AssistantEarlierQuokka"))
	saveHistoryLine(t, dir, conv)

	d := openTUI(t, true, "")
	first := f.nextStream(t, 5*time.Second)
	d.send("UserNewMessageOtter")
	first.push(turnStateFrame(conv, commonpb.TurnState_TURN_STATE_WORKING))
	first.push(streamTextFrames(conv, "item-predrop", preDrop)...)
	d.waitView("the pre-drop stream text", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, preDrop)
	})

	// The turn finishes while the stream is down.
	f.appendHistory(conv, assistantEntry(final))
	f.setTurnState(commonpb.TurnState_TURN_STATE_IDLE)
	first.end()
	d.waitView("the reply from history with no pre-drop text",
		time.Now().Add(10*time.Second), func(v string) bool {
			return strings.Contains(v, final) && !strings.Contains(v, preDrop)
		})
}
