package cmd

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/sycophant-md/sycophant/cmd/syco/internal/config"
	relaypb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/relay/v1"
	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

// testWorkspace is the workspace the test profile names.
const testWorkspace = "w-test"

// fakeGateway plays the relay's client-facing protocol on a loopback port.
// It acks every ChannelStreamReceive with a fresh channel stream ID and then sends only
// the frames a test pushes onto that stream. It acks ChannelStreamIngest with the
// conversation the request names, or with freshID when it names none, and
// from then on that conversation's history holds the user's message. An
// ingest that answers a ServerRequest is acked as it is and touches no
// history. Every ingest is recorded. It
// answers GetConversationHistory with the scripted entries for conversations
// it knows and NotFound for any other ID, and GetTurnState with the scripted
// state (IDLE unless a test sets another). It records every CancelTurn and
// answers it cancelled, or with the error a test sets; it sends no frame.
type fakeGateway struct {
	relaypb.UnimplementedRelayGatewayServer

	addr string

	mu             sync.Mutex
	history        map[string][]*commonpb.HistoryEntry
	turnState      commonpb.TurnState
	freshID        string
	channelStreams int

	streamCh chan *fakeStream
	ingestCh chan struct{}

	// responseCh carries each ingest that answers a ServerRequest.
	ingests    []*commonpb.ChannelStreamIngestRequest
	responseCh chan *commonpb.ChannelStreamIngestRequest

	cancelCh  chan *commonpb.CancelTurnRequest
	cancelErr error

	receiveFails    int
	receiveReason   string
	receiveAttempts int
}

// fakeStream is one open ChannelStreamReceive. Frames pushed onto it are sent in
// order; end() closes the stream after every pushed frame has gone out.
type fakeStream struct {
	channelStreamID string
	frames          chan *commonpb.ChannelStreamOutbound
	endOnce         sync.Once
}

func (s *fakeStream) push(frames ...*commonpb.ChannelStreamOutbound) {
	for _, f := range frames {
		s.frames <- f
	}
}

func (s *fakeStream) end() {
	s.endOnce.Do(func() { close(s.frames) })
}

func startFakeGateway(t *testing.T) *fakeGateway {
	t.Helper()
	lis, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	f := &fakeGateway{
		addr:       lis.Addr().String(),
		history:    make(map[string][]*commonpb.HistoryEntry),
		turnState:  commonpb.TurnState_TURN_STATE_IDLE,
		freshID:    "conv-fresh",
		streamCh:   make(chan *fakeStream, 16),
		ingestCh:   make(chan struct{}, 16),
		responseCh: make(chan *commonpb.ChannelStreamIngestRequest, 16),
		cancelCh:   make(chan *commonpb.CancelTurnRequest, 16),
	}
	srv := grpc.NewServer()
	relaypb.RegisterRelayGatewayServer(srv, f)
	go func() { _ = srv.Serve(lis) }()
	t.Cleanup(srv.Stop)
	return f
}

// setHistory makes the fake know conversation id with exactly these entries.
func (f *fakeGateway) setHistory(id string, entries ...*commonpb.HistoryEntry) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.history[id] = append([]*commonpb.HistoryEntry(nil), entries...)
}

// appendHistory adds entries to a conversation the fake already knows.
func (f *fakeGateway) appendHistory(id string, entries ...*commonpb.HistoryEntry) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.history[id] = append(f.history[id], entries...)
}

func (f *fakeGateway) setTurnState(s commonpb.TurnState) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.turnState = s
}

func (f *fakeGateway) setFreshID(id string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.freshID = id
}

// nextStream waits for the next ChannelStreamReceive the client opens.
func (f *fakeGateway) nextStream(t *testing.T, timeout time.Duration) *fakeStream {
	t.Helper()
	select {
	case s := <-f.streamCh:
		return s
	case <-time.After(timeout):
		t.Fatalf("client opened no ChannelStreamReceive within %v", timeout)
		return nil
	}
}

// nextIngest waits for the next ChannelStreamIngest the client sends.
func (f *fakeGateway) nextIngest(t *testing.T, timeout time.Duration) {
	t.Helper()
	select {
	case <-f.ingestCh:
	case <-time.After(timeout):
		t.Fatalf("client sent no ChannelStreamIngest within %v", timeout)
	}
}

// ingestLog is a copy of every ChannelStreamIngest request so far, in order.
func (f *fakeGateway) ingestLog() []*commonpb.ChannelStreamIngestRequest {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]*commonpb.ChannelStreamIngestRequest(nil), f.ingests...)
}

// nextResponse waits for the next ingest that answers a ServerRequest.
func (f *fakeGateway) nextResponse(t *testing.T, timeout time.Duration) *commonpb.ChannelStreamIngestRequest {
	t.Helper()
	select {
	case req := <-f.responseCh:
		return req
	case <-time.After(timeout):
		t.Fatalf("client sent no ClientResponse within %v", timeout)
		return nil
	}
}

// noResponse fails the test if the client answers a ServerRequest within d.
func (f *fakeGateway) noResponse(t *testing.T, d time.Duration) {
	t.Helper()
	select {
	case req := <-f.responseCh:
		t.Fatalf("client sent an unexpected ClientResponse: %v", req.GetClientResponse())
	case <-time.After(d):
	}
}

// setCancelErr makes every later CancelTurn fail with err.
func (f *fakeGateway) setCancelErr(err error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.cancelErr = err
}

// nextCancel waits for the next CancelTurn the client sends.
func (f *fakeGateway) nextCancel(t *testing.T, timeout time.Duration) *commonpb.CancelTurnRequest {
	t.Helper()
	select {
	case req := <-f.cancelCh:
		return req
	case <-time.After(timeout):
		t.Fatalf("client sent no CancelTurn within %v", timeout)
		return nil
	}
}

// noCancel fails the test if the client sends CancelTurn within d.
func (f *fakeGateway) noCancel(t *testing.T, d time.Duration) {
	t.Helper()
	select {
	case req := <-f.cancelCh:
		t.Fatalf("client sent an unexpected CancelTurn: %v", req)
	case <-time.After(d):
	}
}

func (f *fakeGateway) CancelTurn(
	_ context.Context,
	req *commonpb.CancelTurnRequest,
) (*commonpb.CancelTurnResponse, error) {
	f.mu.Lock()
	err := f.cancelErr
	f.mu.Unlock()
	f.cancelCh <- req
	if err != nil {
		return nil, err
	}
	return &commonpb.CancelTurnResponse{Cancelled: true}, nil
}

// failReceives makes the next n ChannelStreamReceive calls fail
// Unavailable with reason before any ack.
func (f *fakeGateway) failReceives(n int, reason string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.receiveFails = n
	f.receiveReason = reason
}

// attempts is how many ChannelStreamReceive calls the fake has had.
func (f *fakeGateway) attempts() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.receiveAttempts
}

func (f *fakeGateway) ChannelStreamReceive(
	_ *commonpb.ChannelStreamReceiveRequest,
	stream grpc.ServerStreamingServer[commonpb.ChannelStreamOutbound],
) error {
	f.mu.Lock()
	f.receiveAttempts++
	if f.receiveFails > 0 {
		f.receiveFails--
		reason := f.receiveReason
		f.mu.Unlock()
		return status.Error(codes.Unavailable, reason)
	}
	f.channelStreams++
	s := &fakeStream{
		channelStreamID: fmt.Sprintf("chan-%d", f.channelStreams),
		frames:          make(chan *commonpb.ChannelStreamOutbound, 64),
	}
	f.mu.Unlock()

	ack := &commonpb.ChannelStreamOutbound{
		Command: &commonpb.ChannelStreamOutbound_Ack{Ack: &commonpb.ChannelStreamAck{ChannelStreamId: s.channelStreamID}},
	}
	if err := stream.Send(ack); err != nil {
		return err
	}
	f.streamCh <- s
	for {
		select {
		case <-stream.Context().Done():
			return nil
		case frame, ok := <-s.frames:
			if !ok {
				return nil
			}
			if err := stream.Send(frame); err != nil {
				return err
			}
		}
	}
}

func (f *fakeGateway) ChannelStreamIngest(
	_ context.Context,
	req *commonpb.ChannelStreamIngestRequest,
) (*commonpb.ChannelStreamIngestAck, error) {
	f.mu.Lock()
	f.ingests = append(f.ingests, req)
	if req.GetClientResponse() != nil {
		f.mu.Unlock()
		f.responseCh <- req
		return &commonpb.ChannelStreamIngestAck{ChannelStreamId: req.GetChannelStreamId(), ConversationId: req.GetConversationId()}, nil
	}
	id := req.GetConversationId()
	if id == "" {
		id = f.freshID
		f.history[id] = nil
	}
	if _, known := f.history[id]; known && req.GetUserMessage() != nil {
		f.history[id] = append(f.history[id], &commonpb.HistoryEntry{
			Message: &commonpb.Message{Role: "user", Content: req.GetUserMessage().GetContent()},
		})
	}
	f.mu.Unlock()
	f.ingestCh <- struct{}{}
	return &commonpb.ChannelStreamIngestAck{ChannelStreamId: req.GetChannelStreamId(), ConversationId: id}, nil
}

func (f *fakeGateway) GetConversationHistory(
	_ context.Context,
	req *commonpb.GetConversationHistoryRequest,
) (*commonpb.GetConversationHistoryResponse, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	entries, known := f.history[req.GetConversationId()]
	if !known {
		return nil, status.Error(codes.NotFound, "conversation_id not found")
	}
	out := make([]*commonpb.HistoryEntry, len(entries))
	copy(out, entries)
	return &commonpb.GetConversationHistoryResponse{Entries: out}, nil
}

func (f *fakeGateway) GetTurnState(
	_ context.Context,
	req *commonpb.GetTurnStateRequest,
) (*commonpb.TurnStateEvent, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return &commonpb.TurnStateEvent{State: f.turnState, ConversationId: req.GetConversationId()}, nil
}

// --- Protocol values ---

func textContent(text string) []*commonpb.ContentBlock {
	return []*commonpb.ContentBlock{{Block: &commonpb.ContentBlock_Text{Text: &commonpb.TextBlock{Text: text}}}}
}

func userEntry(text string) *commonpb.HistoryEntry {
	return &commonpb.HistoryEntry{Message: &commonpb.Message{Role: "user", Content: textContent(text)}}
}

func assistantEntry(text string) *commonpb.HistoryEntry {
	return &commonpb.HistoryEntry{Message: &commonpb.Message{Role: "assistant", Content: textContent(text)}}
}

func turnStateFrame(conversationID string, state commonpb.TurnState) *commonpb.ChannelStreamOutbound {
	return &commonpb.ChannelStreamOutbound{
		Command: &commonpb.ChannelStreamOutbound_TurnState{
			TurnState: &commonpb.TurnStateEvent{State: state, ConversationId: conversationID},
		},
	}
}

func failedTurnFrame(conv, reason string) *commonpb.ChannelStreamOutbound {
	f := turnStateFrame(conv, commonpb.TurnState_TURN_STATE_FAILED)
	f.GetTurnState().Reason = reason
	return f
}

func serverRequestFrame(requestID, method, paramsJSON string) *commonpb.ChannelStreamOutbound {
	return &commonpb.ChannelStreamOutbound{
		Command: &commonpb.ChannelStreamOutbound_ServerRequest{
			ServerRequest: &commonpb.ServerRequest{RequestId: requestID, Method: method, ParamsJson: paramsJSON},
		},
	}
}

// streamTextFrames is one streamed text item for a conversation: start,
// one delta carrying text, stop.
func streamTextFrames(conversationID, itemID, text string) []*commonpb.ChannelStreamOutbound {
	return []*commonpb.ChannelStreamOutbound{
		streamItemFrame(conversationID, itemID, func(si *commonpb.StreamItem) {
			si.Phase = &commonpb.StreamItem_Start{Start: &commonpb.ItemStart{
				Kind: &commonpb.ItemStart_Text{Text: &commonpb.TextItem{}},
			}}
		}),
		streamItemFrame(conversationID, itemID, func(si *commonpb.StreamItem) {
			si.Phase = &commonpb.StreamItem_Delta{Delta: &commonpb.ItemDelta{
				Kind: &commonpb.ItemDelta_TextDelta{TextDelta: text},
			}}
		}),
		itemStopFrame(conversationID, itemID),
	}
}

func streamItemFrame(conv, itemID string, phase func(*commonpb.StreamItem)) *commonpb.ChannelStreamOutbound {
	si := &commonpb.StreamItem{ItemId: itemID, ConversationId: conv}
	phase(si)
	return &commonpb.ChannelStreamOutbound{Command: &commonpb.ChannelStreamOutbound_StreamItem{StreamItem: si}}
}

func itemStopFrame(conv, itemID string) *commonpb.ChannelStreamOutbound {
	return streamItemFrame(conv, itemID, func(si *commonpb.StreamItem) {
		si.Phase = &commonpb.StreamItem_Stop{Stop: &commonpb.ItemStop{}}
	})
}

func toolStartFrame(conv, itemID, name string) *commonpb.ChannelStreamOutbound {
	return streamItemFrame(conv, itemID, func(si *commonpb.StreamItem) {
		si.Phase = &commonpb.StreamItem_Start{Start: &commonpb.ItemStart{
			Kind: &commonpb.ItemStart_ToolUse{ToolUse: &commonpb.ToolUseItem{Name: name}},
		}}
	})
}

func toolInputFrame(conv, itemID, partial string) *commonpb.ChannelStreamOutbound {
	return streamItemFrame(conv, itemID, func(si *commonpb.StreamItem) {
		si.Phase = &commonpb.StreamItem_Delta{Delta: &commonpb.ItemDelta{
			Kind: &commonpb.ItemDelta_ToolInputJson{ToolInputJson: partial},
		}}
	})
}

func toolCallEntry(id, name, inputJSON string) *commonpb.HistoryEntry {
	return &commonpb.HistoryEntry{Message: &commonpb.Message{
		Role:      "assistant",
		ToolCalls: []*commonpb.ToolCall{{Id: id, Name: name, InputJson: inputJSON}},
	}}
}

const (
	approvalMethod = "RequestToolApproval"
	approvalTool   = "Bash"
	approvalInput  = `{"cmd":"ls /srv/ApprovalInputOcelot"}`
)

// approvalParams is an otherwise valid request's params_json for tool and
// input.
func approvalParams(t *testing.T, tool, input string) string {
	t.Helper()
	b, err := json.Marshal(map[string]string{"tool": tool, "input": input})
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}

// approvalRequest is an otherwise valid approval request for approvalTool
// and approvalInput.
func approvalRequest(t *testing.T, requestID string) *commonpb.ChannelStreamOutbound {
	t.Helper()
	return serverRequestFrame(requestID, approvalMethod, approvalParams(t, approvalTool, approvalInput))
}

// --- Client environment ---

// newClientEnv starts a fake gateway and points a fresh SYCO_CONFIG_DIR at
// it: one profile naming the fake as server and testWorkspace as workspace,
// with a JWT-shaped token that expires in an hour. Returns the fake and the
// config directory.
func newClientEnv(t *testing.T) (*fakeGateway, string) {
	t.Helper()
	f := startFakeGateway(t)
	dir := t.TempDir()
	t.Setenv("SYCO_CONFIG_DIR", dir)
	t.Setenv("SYCO_PROFILE", "")
	t.Setenv("SYCO_SERVER", "")
	t.Setenv("SYCO_WORKSPACE", "")

	cfg := fmt.Sprintf("[default]\nprofile = \"test\"\n\n[profile.test]\nserver = %q\nworkspace = %q\n",
		f.addr, testWorkspace)
	if err := os.WriteFile(filepath.Join(dir, "config.toml"), []byte(cfg), 0o644); err != nil {
		t.Fatal(err)
	}
	creds := fmt.Sprintf("[test]\ntoken = %q\n", futureToken(t))
	if err := os.WriteFile(filepath.Join(dir, "credentials.toml"), []byte(creds), 0o600); err != nil {
		t.Fatal(err)
	}
	return f, dir
}

func futureToken(t *testing.T) string {
	t.Helper()
	enc := base64.RawURLEncoding
	payload, err := json.Marshal(map[string]any{"sub": "tester", "exp": time.Now().Add(time.Hour).Unix()})
	if err != nil {
		t.Fatal(err)
	}
	return enc.EncodeToString([]byte(`{"alg":"none"}`)) + "." + enc.EncodeToString(payload) + ".sig"
}

// saveHistoryLine appends a history line for the test workspace with the
// client's own history writer.
func saveHistoryLine(t *testing.T, dir, conversationID string) {
	t.Helper()
	if err := config.AppendHistory(dir, conversationID, testWorkspace); err != nil {
		t.Fatal(err)
	}
}

// makeLeftoverConfig adds what an earlier cluster leaves behind: a message
// number cursor of 727 for the workspace and a newest history line naming
// a conversation the fake does not know.
func makeLeftoverConfig(t *testing.T, dir, unknownID string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Join(dir, "cursors"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "cursors", testWorkspace+".seq"), []byte("727\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	saveHistoryLine(t, dir, unknownID)
}

// --- Running the chat command ---

type syncBuffer struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (b *syncBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Write(p)
}

func (b *syncBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.String()
}

// chatRun is one `syco chat ...` executing through the root command in the
// background.
type chatRun struct {
	stdout, stderr *syncBuffer
	done           chan error
}

func startChat(args ...string) *chatRun {
	return startChatWithStdin(nil, args...)
}

func startChatWithStdin(stdin io.Reader, args ...string) *chatRun {
	r := &chatRun{stdout: &syncBuffer{}, stderr: &syncBuffer{}, done: make(chan error, 1)}
	root := Root()
	root.SetArgs(append([]string{"chat"}, args...))
	root.SetIn(stdin)
	root.SetOut(r.stdout)
	root.SetErr(r.stderr)
	go func() { r.done <- root.Execute() }()
	return r
}

// wait returns the command's error, or fails the test if it has not
// returned within timeout.
func (r *chatRun) wait(t *testing.T, timeout time.Duration) error {
	t.Helper()
	select {
	case err := <-r.done:
		return err
	case <-time.After(timeout):
		t.Fatalf("chat command did not return within %v\nstdout: %q\nstderr: %q",
			timeout, r.stdout.String(), r.stderr.String())
		return nil
	}
}
