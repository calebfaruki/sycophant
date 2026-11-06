// The tests in this file cover the per-message grant choice being gone: the
// agent's provision is the only source of access, so a message carries no
// access of its own.

package cmd

import (
	"testing"

	"google.golang.org/protobuf/proto"

	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

// `chat` offers no `--grant` flag: a sender cannot pick a Secret per message.
func TestChatHasNoGrantFlag(t *testing.T) {
	if f := newChatCmd().Flags().Lookup("grant"); f != nil {
		t.Fatalf("chat still defines --grant: %+v", f)
	}
}

// A user message carries its text and nothing else: no grant selection, or any
// other field, rides along with it.
func TestUserMessageIngestCarriesOnlyTheText(t *testing.T) {
	req := userMessageIngest("cs-1", "conv-1", "hello")

	want := &commonpb.UserMessage{
		Content: []*commonpb.ContentBlock{{Block: &commonpb.ContentBlock_Text{Text: &commonpb.TextBlock{Text: "hello"}}}},
	}
	if !proto.Equal(req.GetUserMessage(), want) {
		t.Fatalf("user message = %v, want only its text %v", req.GetUserMessage(), want)
	}
	if req.GetChannelStreamId() != "cs-1" || req.GetConversationId() != "conv-1" {
		t.Fatalf("ingest ids = (%q, %q), want (cs-1, conv-1)", req.GetChannelStreamId(), req.GetConversationId())
	}
}
