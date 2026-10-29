package cmd

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log"
	"os"
	"strings"
	"time"

	"charm.land/glamour/v2"
	"charm.land/lipgloss/v2"
	"github.com/mattn/go-isatty"
	"github.com/spf13/cobra"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/sycophant-md/sycophant/cmd/syco/internal/config"
	"github.com/sycophant-md/sycophant/cmd/syco/internal/grpcclient"
	relaypb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/relay/v1"
	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

const (
	// heartbeatInterval matches the relay's server-side heartbeat interval.
	heartbeatInterval = 15 * time.Second
	// deadConnectionTimeout triggers a reconnect when no frame arrives within
	// 2× the heartbeat interval.
	deadConnectionTimeout = 2 * heartbeatInterval
	initialBackoff        = time.Second
	// maxBackoff caps the exponential reconnect delay.
	maxBackoff = 30 * time.Second
)

func newChatCmd() *cobra.Command {
	var (
		continueFlag bool
		resumeID     string
		grants       []string
	)

	cmd := &cobra.Command{
		Use:   "chat [message]",
		Short: "Send a message and stream the reply",
		Args:  cobra.MaximumNArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			cfgDir := config.ConfigDir()
			profileFlag, _ := cmd.Flags().GetString("profile")
			p, err := config.LoadProfile(cfgDir, profileFlag, "")
			if err != nil {
				return err
			}

			token, err := config.LoadToken(cfgDir, p.Name)
			if err != nil {
				return err
			}

			// Parse grants (shared by both paths).
			var grantSelections []*commonpb.GrantSelection
			for _, g := range grants {
				toolset, grant, err := config.ParseGrant(g)
				if err != nil {
					return err
				}
				grantSelections = append(grantSelections, &commonpb.GrantSelection{
					Toolset: toolset,
					Grant:   grant,
				})
			}

			// Interactive TUI mode when no message argument is given.
			// The TUI handles workspace selection if none is set.
			if len(args) == 0 {
				return runTUI(cfgDir, p, token, continueFlag, resumeID, grantSelections)
			}

			// One-shot mode requires a workspace.
			if p.Workspace == "" {
				return fmt.Errorf("workspace is required; set it with `syco session workspace <ws>`")
			}

			// One-shot mode.
			message := args[0]

			// Resolve conversation ID
			var conversationID string
			if continueFlag {
				entry, err := config.HistoryLastForWorkspace(cfgDir, p.Workspace)
				if err != nil {
					return err
				}
				conversationID = entry.ConversationID
			} else if resumeID != "" {
				conversationID = resumeID
			}

			ctx := grpcclient.WithToken(context.Background(), token)

			conn, err := grpcclient.Dial(ctx, p.Server)
			if err != nil {
				return fmt.Errorf("connecting to relay: %w", err)
			}
			defer func() { conn.Close() }()

			client := relaypb.NewRelayGatewayClient(conn)

			// A named conversation must exist for this user before anything
			// is sent to it.
			if conversationID != "" {
				if _, err := loadHistory(ctx, client, conversationID, p.Workspace); err != nil {
					return err
				}
			}

			stream, err := client.ChannelStreamReceive(ctx, &commonpb.ChannelStreamReceiveRequest{Workspace: p.Workspace})
			if err != nil {
				return fmt.Errorf("channel stream receive: %w", err)
			}

			channelStreamID, err := readAck(stream)
			if err != nil {
				return fmt.Errorf("reading channel stream ack: %w", err)
			}

			// Ingest the message (once, on first connect).
			ingestResp, err := client.ChannelStreamIngest(ctx, userMessageIngest(channelStreamID, conversationID, message, grantSelections))
			if err != nil {
				return fmt.Errorf("channel stream ingest: %w", err)
			}
			conversationID = ingestResp.ConversationId
			if conversationID != "" {
				if err := config.AppendHistory(cfgDir, conversationID, p.Workspace); err != nil {
					log.Printf("warning: saving conversation history: %v", err)
				}
			}

			// Stream the reply with reconnect support.
			out := cmd.OutOrStdout()
			rs := &recvState{
				workspace:       p.Workspace,
				conversationID:  conversationID,
				channelStreamID: channelStreamID,
				out:             out,
				errOut:          cmd.ErrOrStderr(),
				in:              cmd.InOrStdin(),
				isTTY:           isTerminal(out),
				toolStyle:       lipgloss.NewStyle().Foreground(lipgloss.Color("6")),
				toolCalls:       make(map[string]string),
			}

			backoff := initialBackoff
			for {
				result, err := rs.recvLoop(ctx, client, stream)
				switch result {
				case loopDone:
					return nil
				case loopReconnect:
					// Fall through to reconnect.
				default:
					if err != nil {
						return err
					}
				}

				// Reconnect with exponential backoff.
				log.Printf("stream disconnected, reconnecting in %v…", backoff)
				time.Sleep(backoff)
				backoff = min(2*backoff, maxBackoff)

				// Re-dial in case the connection is dead.
				conn.Close()
				conn, err = grpcclient.Dial(ctx, p.Server)
				if err != nil {
					return fmt.Errorf("reconnecting to relay: %w", err)
				}
				client = relaypb.NewRelayGatewayClient(conn)

				stream, err = client.ChannelStreamReceive(ctx, &commonpb.ChannelStreamReceiveRequest{Workspace: p.Workspace})
				if err != nil {
					return fmt.Errorf("channel stream receive on reconnect: %w", err)
				}
				rs.channelStreamID, err = readAck(stream)
				if err != nil {
					return fmt.Errorf("reading channel stream ack on reconnect: %w", err)
				}

				// Frames sent during the drop are gone. The reply now comes
				// from history once the turn is over, never from streamed text.
				rs.reconnected = true
				ts, err := getTurnState(ctx, client, conversationID, p.Workspace)
				if err != nil {
					return err
				}
				switch ts.GetState() {
				case commonpb.TurnState_TURN_STATE_WORKING:
					// Keep listening until the turn ends.
				case commonpb.TurnState_TURN_STATE_FAILED:
					return fmt.Errorf("turn failed: %s", ts.GetReason())
				case commonpb.TurnState_TURN_STATE_CANCELLED:
					return errTurnCancelled
				default:
					return rs.printReplyFromHistory(ctx, client)
				}
			}
		},
	}

	cmd.Flags().BoolVar(&continueFlag, "continue", false, "Continue the last conversation for the current workspace")
	cmd.Flags().StringVar(&resumeID, "resume", "", "Resume a specific conversation by ID")
	cmd.Flags().StringArrayVar(&grants, "grant", nil, "Grant selection (toolset/grant)")
	return cmd
}

var errTurnCancelled = errors.New("turn cancelled")

// isTerminal reports whether w is the process's stdout attached to a
// terminal. Only then is output styled.
func isTerminal(w io.Writer) bool {
	f, ok := w.(*os.File)
	return ok && f == os.Stdout && isatty.IsTerminal(f.Fd())
}

// --- Conversation reads ---

// loadHistory reads a conversation's whole history. An ID the server does
// not know for this user is reported by ID alone.
func loadHistory(ctx context.Context, client relaypb.RelayGatewayClient, conversationID, workspace string) ([]*commonpb.HistoryEntry, error) {
	resp, err := client.GetConversationHistory(ctx, &commonpb.GetConversationHistoryRequest{
		ConversationId: conversationID,
		Workspace:      workspace,
	})
	if err != nil {
		if status.Code(err) == codes.NotFound {
			return nil, fmt.Errorf("conversation %s not found", conversationID)
		}
		return nil, fmt.Errorf("loading conversation %s: %w", conversationID, err)
	}
	return resp.GetEntries(), nil
}

// getTurnState asks the relay for a conversation's current turn state.
func getTurnState(ctx context.Context, client relaypb.RelayGatewayClient, conversationID, workspace string) (*commonpb.TurnStateEvent, error) {
	ts, err := client.GetTurnState(ctx, &commonpb.GetTurnStateRequest{
		ConversationId: conversationID,
		Workspace:      workspace,
	})
	if err != nil {
		return nil, fmt.Errorf("getting turn state for conversation %s: %w", conversationID, err)
	}
	return ts, nil
}

// entryText concatenates a history entry's text blocks.
func entryText(msg *commonpb.Message) string {
	var b strings.Builder
	for _, block := range msg.GetContent() {
		if tb := block.GetText(); tb != nil {
			b.WriteString(tb.Text)
		}
	}
	return b.String()
}

// replyFromHistory is the text of the latest turn: every untagged assistant
// entry after the last untagged user entry.
func replyFromHistory(entries []*commonpb.HistoryEntry) string {
	last := -1
	for i, e := range entries {
		if e.Tag == nil && e.GetMessage().GetRole() == "user" {
			last = i
		}
	}
	var b strings.Builder
	for _, e := range entries[last+1:] {
		if e.Tag != nil || e.GetMessage().GetRole() != "assistant" {
			continue
		}
		b.WriteString(entryText(e.GetMessage()))
	}
	return b.String()
}

// --- Stream helpers ---

func readAck(stream grpc.ServerStreamingClient[commonpb.ChannelStreamOutbound]) (string, error) {
	firstFrame, err := stream.Recv()
	if err != nil {
		return "", err
	}
	ack := firstFrame.GetAck()
	if ack == nil {
		return "", fmt.Errorf("expected ChannelStreamAck as first frame")
	}
	return ack.ChannelStreamId, nil
}

type loopResult int

const (
	loopDone      loopResult = iota // Turn finished (IDLE).
	loopReconnect                   // Stream broke; reconnect.
	loopFatal                       // Unrecoverable error.
	loopContinue  loopResult = -1   // Keep receiving frames.
)

// recvState holds mutable state across the receive loop and reconnects.
type recvState struct {
	workspace      string
	conversationID string
	out            io.Writer
	errOut         io.Writer
	isTTY          bool
	toolStyle      lipgloss.Style
	textBuf        strings.Builder
	toolCalls      map[string]string // item_id -> tool name
	// reconnected is set once a stream has dropped: streamed text is then
	// discarded and the reply is read from history.
	reconnected bool

	// channelStreamID is the open stream, where approval answers go.
	channelStreamID string
	in              io.Reader
	// lines reads in from the first approval request on, so a turn with
	// none never touches standard input.
	lines <-chan string
	// approvals wait for a line each, first in, first out.
	approvals []toolApproval
}

// recvLoop reads frames until the turn completes, the stream breaks, or the
// dead-connection timer fires.
func (rs *recvState) recvLoop(
	ctx context.Context,
	client relaypb.RelayGatewayClient,
	stream grpc.ServerStreamingClient[commonpb.ChannelStreamOutbound],
) (loopResult, error) {
	timer := time.NewTimer(deadConnectionTimeout)
	defer timer.Stop()

	frameCh := make(chan frameOrErr, 1)
	go func() {
		for {
			frame, err := stream.Recv()
			frameCh <- frameOrErr{frame: frame, err: err}
			if err != nil {
				return
			}
		}
	}()

	for {
		var lines <-chan string
		if len(rs.approvals) > 0 {
			lines = rs.lines
		}
		select {
		case <-ctx.Done():
			return loopFatal, ctx.Err()
		case line, ok := <-lines:
			rs.answerApproval(ctx, client, ok && approvesLine(line))
		case <-timer.C:
			log.Printf("dead connection detected (no frame in %v)", deadConnectionTimeout)
			return loopReconnect, nil
		case fe := <-frameCh:
			if fe.err != nil {
				if fe.err == io.EOF {
					return loopReconnect, nil
				}
				log.Printf("stream error: %v", fe.err)
				return loopReconnect, nil
			}
			frame := fe.frame

			// Reset dead-connection timer on every frame (including heartbeat).
			timer.Reset(deadConnectionTimeout)

			result, err := rs.handleFrame(ctx, client, frame)
			if result != loopContinue {
				return result, err
			}
		}
	}
}

type frameOrErr struct {
	frame *commonpb.ChannelStreamOutbound
	err   error
}

func (rs *recvState) handleFrame(
	ctx context.Context,
	client relaypb.RelayGatewayClient,
	frame *commonpb.ChannelStreamOutbound,
) (loopResult, error) {
	switch c := frame.Command.(type) {
	case *commonpb.ChannelStreamOutbound_TurnState:
		if c.TurnState.ConversationId != rs.conversationID {
			return loopContinue, nil
		}
		switch c.TurnState.State {
		case commonpb.TurnState_TURN_STATE_IDLE:
			if rs.reconnected {
				if err := rs.printReplyFromHistory(ctx, client); err != nil {
					return loopFatal, err
				}
				return loopDone, nil
			}
			flushText(rs.out, &rs.textBuf, rs.isTTY)
			fmt.Fprintf(rs.errOut, "conversation: %s\n", rs.conversationID)
			return loopDone, nil
		case commonpb.TurnState_TURN_STATE_FAILED:
			return loopFatal, fmt.Errorf("turn failed: %s", c.TurnState.Reason)
		case commonpb.TurnState_TURN_STATE_CANCELLED:
			return loopFatal, errTurnCancelled
		}

	case *commonpb.ChannelStreamOutbound_StreamItem:
		item := c.StreamItem
		if item.ConversationId != rs.conversationID {
			return loopContinue, nil
		}
		rs.renderStreamItem(item)

	case *commonpb.ChannelStreamOutbound_ServerRequest:
		if a, ok := toolApprovalOf(c.ServerRequest); ok {
			rs.askApproval(a)
		}
	}

	return loopContinue, nil
}

// askApproval shows an approval request and queues it for a line of input.
func (rs *recvState) askApproval(a toolApproval) {
	rs.approvals = append(rs.approvals, a)
	if rs.lines == nil {
		rs.lines = readLines(rs.in)
	}
	rs.printToolLine(fmt.Sprintf("⚙ %s wants to run with input: %s", a.tool, a.input))
	fmt.Fprintln(rs.out, "approve? [y/N]")
}

// answerApproval sends the answer to the oldest queued approval request. A
// failed send leaves the call unanswered, which the harness takes as a no.
func (rs *recvState) answerApproval(ctx context.Context, client relaypb.RelayGatewayClient, approved bool) {
	a := rs.approvals[0]
	rs.approvals = rs.approvals[1:]
	if _, err := client.ChannelStreamIngest(ctx, approvalAnswer(rs.channelStreamID, a.requestID, approved)); err != nil {
		log.Printf("sending approval answer: %v", err)
	}
}

// printReplyFromHistory prints the finished turn's reply as read from the
// conversation history, then the conversation line.
func (rs *recvState) printReplyFromHistory(ctx context.Context, client relaypb.RelayGatewayClient) error {
	entries, err := loadHistory(ctx, client, rs.conversationID, rs.workspace)
	if err != nil {
		return err
	}
	rs.textBuf.Reset()
	rs.textBuf.WriteString(replyFromHistory(entries))
	flushText(rs.out, &rs.textBuf, rs.isTTY)
	fmt.Fprintf(rs.errOut, "conversation: %s\n", rs.conversationID)
	return nil
}

// renderStreamItem displays a single stream item.
func (rs *recvState) renderStreamItem(item *commonpb.StreamItem) {
	switch phase := item.Phase.(type) {
	case *commonpb.StreamItem_Start:
		if tu := phase.Start.GetToolUse(); tu != nil {
			rs.toolCalls[item.ItemId] = tu.Name
			rs.printToolLine(fmt.Sprintf("⚙ %s (started)", tu.Name))
		}
	case *commonpb.StreamItem_Delta:
		if delta := phase.Delta.GetTextDelta(); delta != "" && !rs.reconnected {
			rs.textBuf.WriteString(delta)
		}
	case *commonpb.StreamItem_Stop:
		if name, ok := rs.toolCalls[item.ItemId]; ok {
			rs.printToolLine(fmt.Sprintf("⚙ %s (finished)", name))
		}
	}
}

func (rs *recvState) printToolLine(line string) {
	if rs.isTTY {
		// lipgloss.Fprintln downsamples colors to what the terminal supports.
		lipgloss.Fprintln(rs.out, rs.toolStyle.Render(line))
		return
	}
	fmt.Fprintln(rs.out, line)
}

func flushText(w io.Writer, buf *strings.Builder, isTTY bool) {
	text := buf.String()
	if text == "" {
		return
	}
	if isTTY {
		rendered, err := glamour.Render(text, "dark")
		if err == nil {
			lipgloss.Fprint(w, rendered)
		} else {
			fmt.Fprint(w, text)
		}
	} else {
		fmt.Fprint(w, text)
	}
	buf.Reset()
}
