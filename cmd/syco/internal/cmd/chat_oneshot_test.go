package cmd

import (
	"slices"
	"strings"
	"testing"
	"time"

	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

// The agent's conversation is shared, so the stream carries other
// people's turns too. A one-shot prints only its own turn's reply and
// returns only when its own turn ends.
func TestOneShotIgnoresAnotherMessagesTurnAndEndsWithItsOwn(t *testing.T) {
	const (
		ownID      = "msg-own-oneshot"
		otherID    = "msg-other-oneshot"
		ownReply   = "AssistantOwnReplyOtter"
		otherReply = "AssistantOtherReplyNarwhal"
	)
	f, _ := newClientEnv(t)
	f.setMessageID(ownID)

	run := startChat("UserOwnMessageOtter")
	stream := f.nextStream(t, 5*time.Second)
	f.nextIngest(t, 5*time.Second)
	stream.push(turnStateFrame(otherID, commonpb.TurnState_TURN_STATE_WORKING))
	stream.push(streamTextFrames(otherID, "item-other", otherReply)...)
	stream.push(turnStateFrame(otherID, commonpb.TurnState_TURN_STATE_IDLE))
	stream.push(turnStateFrame(ownID, commonpb.TurnState_TURN_STATE_WORKING))
	stream.push(streamTextFrames(ownID, "item-own", ownReply)...)
	stream.push(turnStateFrame(ownID, commonpb.TurnState_TURN_STATE_IDLE))

	if err := run.wait(t, 10*time.Second); err != nil {
		t.Fatalf("one-shot returned %v, want nil\nstderr: %s", err, run.stderr.String())
	}
	out := run.stdout.String()
	if !strings.Contains(out, ownReply) {
		t.Errorf("stdout lacks the command's own reply %q (it returned at the other turn's end?): %q", ownReply, out)
	}
	if strings.Contains(out, otherReply) {
		t.Errorf("stdout holds another message's reply %q: %q", otherReply, out)
	}
	if receives, _ := f.agentsNamed(); !slices.Equal(receives, []string{testAgent}) {
		t.Errorf("ChannelStreamReceive named agents %q, want [%q]", receives, testAgent)
	}
	var sent []string
	for _, req := range f.ingestLog() {
		if req.GetUserMessage() != nil {
			sent = append(sent, req.GetChannelStreamId())
		}
	}
	if !slices.Equal(sent, []string{stream.channelStreamID}) {
		t.Errorf("user message ingests named channel streams %q, want [%q]", sent, stream.channelStreamID)
	}
}

func TestOneShotReturnsTheConnectError(t *testing.T) {
	f, _ := newClientEnv(t)
	f.failReceives(1, "GatewayDownX")

	err := startChat("UserMessageHeron").wait(t, 10*time.Second)
	if err == nil || !strings.Contains(err.Error(), "GatewayDownX") {
		t.Fatalf("one-shot returned %v, want the connect error naming GatewayDownX", err)
	}
}
