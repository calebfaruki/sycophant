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

	relaypb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/relay/v1"
	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

// testAgent is the agent the test profile names.
const testAgent = "a-test"

// fakeGateway plays the relay's client-facing protocol on a loopback port.
// It acks every ChannelStreamReceive with a fresh channel stream ID and then sends only
// the frames a test pushes onto that stream. It acks a user message's
// ChannelStreamIngest with messageID, and from then on the agent's one
// conversation holds the user's message. An ingest that answers a
// ServerRequest is acked as it is and touches no history. Every ingest is
// recorded. It answers GetConversationHistory with the agent's entries and
// GetTurnState IDLE.
// It records every CancelTurn and answers it cancelled, every
// TakeBackMessage and answers it taken false, and every ClearContext and
// CompactContext: clear empties the history, compact leaves it. None of
// these sends a frame. These are the defaults; the set*, fail* and hold*
// helpers change them.
type fakeGateway struct {
	relaypb.UnimplementedRelayGatewayServer

	addr string

	mu             sync.Mutex
	history        []*commonpb.HistoryEntry
	turnState      commonpb.TurnState
	messageID      string
	ackHold        chan struct{}
	ingestErr      error
	historyErr     error
	historyHold    chan struct{}
	channelStreams int

	streamCh chan *fakeStream
	ingestCh chan struct{}

	ingests []*commonpb.ChannelStreamIngestRequest
	// responseCh carries each ingest that answers a ServerRequest.
	responseCh chan *commonpb.ChannelStreamIngestRequest

	cancelCh  chan *commonpb.CancelTurnRequest
	cancelErr error

	takeBackCh chan *commonpb.TakeBackMessageRequest
	takeBack   string

	// contextCh carries "clear" or "compact" for each call.
	contextCh  chan string
	contextErr error

	receiveFails    int
	receiveReason   string
	receiveAttempts int

	receiveAgents []string
	historyAgents []string
}

// agentsNamed is a copy of the agent each ChannelStreamReceive and each
// GetConversationHistory named so far, in order.
func (f *fakeGateway) agentsNamed() (receives, histories []string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]string(nil), f.receiveAgents...), append([]string(nil), f.historyAgents...)
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
		turnState:  commonpb.TurnState_TURN_STATE_IDLE,
		messageID:  "msg-fresh",
		streamCh:   make(chan *fakeStream, 16),
		ingestCh:   make(chan struct{}, 16),
		responseCh: make(chan *commonpb.ChannelStreamIngestRequest, 16),
		cancelCh:   make(chan *commonpb.CancelTurnRequest, 16),
		takeBackCh: make(chan *commonpb.TakeBackMessageRequest, 16),
		contextCh:  make(chan string, 16),
	}
	srv := grpc.NewServer()
	relaypb.RegisterRelayGatewayServer(srv, f)
	go func() { _ = srv.Serve(lis) }()
	t.Cleanup(srv.Stop)
	return f
}

// setHistory makes the agent's conversation exactly these entries.
func (f *fakeGateway) setHistory(entries ...*commonpb.HistoryEntry) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.history = append([]*commonpb.HistoryEntry(nil), entries...)
}

// appendHistory adds entries to the agent's conversation.
func (f *fakeGateway) appendHistory(entries ...*commonpb.HistoryEntry) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.history = append(f.history, entries...)
}

func (f *fakeGateway) setTurnState(s commonpb.TurnState) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.turnState = s
}

// setMessageID makes the fake ack every later user message with id.
func (f *fakeGateway) setMessageID(id string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.messageID = id
}

// holdAcks makes every later user message's ack wait until release runs.
// The message is recorded, and in the history, before then.
func (f *fakeGateway) holdAcks() (release func()) {
	hold := make(chan struct{})
	f.mu.Lock()
	defer f.mu.Unlock()
	f.ackHold = hold
	return func() { close(hold) }
}

// failIngests makes every later user message's ingest fail with err. The
// message is recorded but not added to the history.
func (f *fakeGateway) failIngests(err error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.ingestErr = err
}

// failHistory makes every later GetConversationHistory fail with err.
func (f *fakeGateway) failHistory(err error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.historyErr = err
}

// holdHistory makes every later GetConversationHistory wait until release
// runs, then answer with the history as it is then.
func (f *fakeGateway) holdHistory() (release func()) {
	hold := make(chan struct{})
	f.mu.Lock()
	defer f.mu.Unlock()
	f.historyHold = hold
	return func() { close(hold) }
}

// setTakeBack makes the next TakeBackMessage take a message with text.
func (f *fakeGateway) setTakeBack(text string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.takeBack = text
}

// nextTakeBack waits for the next TakeBackMessage the client sends.
func (f *fakeGateway) nextTakeBack(t *testing.T, timeout time.Duration) *commonpb.TakeBackMessageRequest {
	t.Helper()
	select {
	case req := <-f.takeBackCh:
		return req
	case <-time.After(timeout):
		t.Fatalf("client sent no TakeBackMessage within %v", timeout)
		return nil
	}
}

func (f *fakeGateway) TakeBackMessage(
	_ context.Context,
	req *commonpb.TakeBackMessageRequest,
) (*commonpb.TakeBackMessageResponse, error) {
	f.mu.Lock()
	resp := &commonpb.TakeBackMessageResponse{}
	if f.takeBack != "" {
		resp.Taken, resp.Content = true, textContent(f.takeBack)
		f.takeBack = ""
	}
	f.mu.Unlock()
	f.takeBackCh <- req
	return resp, nil
}

// setContextErr makes every later ClearContext and CompactContext fail
// with err.
func (f *fakeGateway) setContextErr(err error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.contextErr = err
}

// nextContext waits for the next ClearContext ("clear") or CompactContext
// ("compact") the client sends.
func (f *fakeGateway) nextContext(t *testing.T, timeout time.Duration) string {
	t.Helper()
	select {
	case kind := <-f.contextCh:
		return kind
	case <-time.After(timeout):
		t.Fatalf("client sent no ClearContext or CompactContext within %v", timeout)
		return ""
	}
}

func (f *fakeGateway) ClearContext(
	_ context.Context,
	_ *commonpb.ClearContextRequest,
) (*commonpb.ClearContextResponse, error) {
	f.mu.Lock()
	err := f.contextErr
	if err == nil {
		f.history = nil
	}
	f.mu.Unlock()
	f.contextCh <- "clear"
	if err != nil {
		return nil, err
	}
	return &commonpb.ClearContextResponse{}, nil
}

func (f *fakeGateway) CompactContext(
	_ context.Context,
	_ *commonpb.CompactContextRequest,
) (*commonpb.CompactContextResponse, error) {
	f.mu.Lock()
	err := f.contextErr
	f.mu.Unlock()
	f.contextCh <- "compact"
	if err != nil {
		return nil, err
	}
	return &commonpb.CompactContextResponse{}, nil
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

// noCancel fails the test if the client sends CancelTurn within d.
func (f *fakeGateway) noCancel(t *testing.T, d time.Duration) {
	t.Helper()
	select {
	case req := <-f.cancelCh:
		t.Fatalf("client sent an unexpected CancelTurn: %v", req)
	case <-time.After(d):
	}
}

// noTakeBack fails if the client sends a TakeBackMessage within d.
func (f *fakeGateway) noTakeBack(t *testing.T, d time.Duration) {
	t.Helper()
	select {
	case <-f.takeBackCh:
		t.Fatalf("client sent a TakeBackMessage within %v", d)
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
	req *commonpb.ChannelStreamReceiveRequest,
	stream grpc.ServerStreamingServer[commonpb.ChannelStreamOutbound],
) error {
	f.mu.Lock()
	f.receiveAttempts++
	f.receiveAgents = append(f.receiveAgents, req.GetAgent())
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
		return &commonpb.ChannelStreamIngestAck{ChannelStreamId: req.GetChannelStreamId()}, nil
	}
	if req.GetUserMessage() != nil {
		if err := f.ingestErr; err != nil {
			f.mu.Unlock()
			f.ingestCh <- struct{}{}
			return nil, err
		}
		f.history = append(f.history, &commonpb.HistoryEntry{
			Message: &commonpb.Message{Role: "user", Content: req.GetUserMessage().GetContent()},
		})
	}
	id, hold := f.messageID, f.ackHold
	f.mu.Unlock()
	f.ingestCh <- struct{}{}
	if hold != nil {
		<-hold
	}
	return &commonpb.ChannelStreamIngestAck{ChannelStreamId: req.GetChannelStreamId(), MessageId: id}, nil
}

func (f *fakeGateway) GetConversationHistory(
	_ context.Context,
	req *commonpb.GetConversationHistoryRequest,
) (*commonpb.GetConversationHistoryResponse, error) {
	f.mu.Lock()
	hold := f.historyHold
	f.mu.Unlock()
	if hold != nil {
		<-hold
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	f.historyAgents = append(f.historyAgents, req.GetAgent())
	if f.historyErr != nil {
		return nil, f.historyErr
	}
	out := make([]*commonpb.HistoryEntry, len(f.history))
	copy(out, f.history)
	return &commonpb.GetConversationHistoryResponse{Entries: out}, nil
}

func (f *fakeGateway) GetTurnState(
	_ context.Context,
	_ *commonpb.GetTurnStateRequest,
) (*commonpb.TurnStateEvent, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return &commonpb.TurnStateEvent{State: f.turnState}, nil
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

// turnStateFrame is a turn state for the turn of the user message messageID.
func turnStateFrame(messageID string, state commonpb.TurnState) *commonpb.ChannelStreamOutbound {
	return &commonpb.ChannelStreamOutbound{
		Command: &commonpb.ChannelStreamOutbound_TurnState{
			TurnState: &commonpb.TurnStateEvent{State: state, MessageId: messageID},
		},
	}
}

func failedTurnFrame(messageID, reason string) *commonpb.ChannelStreamOutbound {
	f := turnStateFrame(messageID, commonpb.TurnState_TURN_STATE_FAILED)
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

// streamTextFrames is one streamed text item of the turn of messageID:
// start, one delta carrying text, stop.
func streamTextFrames(messageID, itemID, text string) []*commonpb.ChannelStreamOutbound {
	return []*commonpb.ChannelStreamOutbound{
		streamItemFrame(messageID, itemID, func(si *commonpb.StreamItem) {
			si.Phase = &commonpb.StreamItem_Start{Start: &commonpb.ItemStart{
				Kind: &commonpb.ItemStart_Text{Text: &commonpb.TextItem{}},
			}}
		}),
		streamItemFrame(messageID, itemID, func(si *commonpb.StreamItem) {
			si.Phase = &commonpb.StreamItem_Delta{Delta: &commonpb.ItemDelta{
				Kind: &commonpb.ItemDelta_TextDelta{TextDelta: text},
			}}
		}),
		itemStopFrame(messageID, itemID),
	}
}

func streamItemFrame(messageID, itemID string, phase func(*commonpb.StreamItem)) *commonpb.ChannelStreamOutbound {
	si := &commonpb.StreamItem{ItemId: itemID, MessageId: messageID}
	phase(si)
	return &commonpb.ChannelStreamOutbound{Command: &commonpb.ChannelStreamOutbound_StreamItem{StreamItem: si}}
}

func itemStopFrame(messageID, itemID string) *commonpb.ChannelStreamOutbound {
	return streamItemFrame(messageID, itemID, func(si *commonpb.StreamItem) {
		si.Phase = &commonpb.StreamItem_Stop{Stop: &commonpb.ItemStop{}}
	})
}

func toolStartFrame(messageID, itemID, name string) *commonpb.ChannelStreamOutbound {
	return streamItemFrame(messageID, itemID, func(si *commonpb.StreamItem) {
		si.Phase = &commonpb.StreamItem_Start{Start: &commonpb.ItemStart{
			Kind: &commonpb.ItemStart_ToolUse{ToolUse: &commonpb.ToolUseItem{Name: name}},
		}}
	})
}

func toolInputFrame(messageID, itemID, partial string) *commonpb.ChannelStreamOutbound {
	return streamItemFrame(messageID, itemID, func(si *commonpb.StreamItem) {
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
// it: one profile naming the fake as server and testAgent as agent,
// with a JWT-shaped token that expires in an hour. Returns the fake and the
// config directory.
func newClientEnv(t *testing.T) (*fakeGateway, string) {
	t.Helper()
	f := startFakeGateway(t)
	dir := t.TempDir()
	t.Setenv("SYCO_CONFIG_DIR", dir)
	t.Setenv("SYCO_PROFILE", "")
	t.Setenv("SYCO_SERVER", "")
	t.Setenv("SYCO_AGENT", "")

	cfg := fmt.Sprintf("[default]\nprofile = \"test\"\n\n[profile.test]\nserver = %q\nagent = %q\n",
		f.addr, testAgent)
	if err := os.WriteFile(filepath.Join(dir, "config.toml"), []byte(cfg), 0o644); err != nil {
		t.Fatal(err)
	}
	creds := fmt.Sprintf("[test]\ntoken = %q\n", futureToken(t, time.Now().Add(time.Hour)))
	if err := os.WriteFile(filepath.Join(dir, "credentials.toml"), []byte(creds), 0o600); err != nil {
		t.Fatal(err)
	}
	return f, dir
}

func futureToken(t *testing.T, exp time.Time) string {
	t.Helper()
	enc := base64.RawURLEncoding
	payload, err := json.Marshal(map[string]any{"sub": "tester", "exp": exp.Unix()})
	if err != nil {
		t.Fatal(err)
	}
	return enc.EncodeToString([]byte(`{"alg":"none"}`)) + "." + enc.EncodeToString(payload) + ".sig"
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
