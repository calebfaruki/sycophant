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

	"github.com/sycophant-md/sycophant/cmd/syco/internal/config"
	"github.com/sycophant-md/sycophant/cmd/syco/internal/grpcclient"
	relaypb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/relay/v1"
	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

const (
	// heartbeatInterval matches the relay's server-side heartbeat interval.
	heartbeatInterval = 15 * time.Second
	// deadConnectionTimeout ends a stream that receives no frame.
	deadConnectionTimeout = 2 * heartbeatInterval
)

func newChatCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "chat [message]",
		Short: "Send a message and stream the reply",
		Args:  cobra.MaximumNArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			cfgDir := config.ConfigDir()
			profileFlag, _ := cmd.Flags().GetString("profile")
			p, err := config.LoadProfile(cfgDir, profileFlag)
			if err != nil {
				return err
			}

			token, err := config.LoadToken(cfgDir, p.Name)
			if err != nil {
				return err
			}

			// The TUI handles agent selection if none is set.
			if len(args) == 0 {
				return runTUI(cfgDir, p, token)
			}

			if p.Agent == "" {
				return fmt.Errorf("agent is required; set it with `syco session agent <agent>`")
			}

			message := args[0]

			ctx := grpcclient.WithToken(context.Background(), token)

			cs, err := connectChannelStream(ctx, p.Server, p.Agent, nil)
			if err != nil {
				return err
			}
			defer cs.conn.Close()

			ingestResp, err := cs.client.ChannelStreamIngest(ctx, userMessageIngest(cs.channelStreamID, message))
			if err != nil {
				return fmt.Errorf("channel stream ingest: %w", err)
			}

			// Stream the reply. If the stream drops the command stops: the
			// agent's conversation is shared, so after a drop this command
			// cannot tell its own reply from someone else's.
			out := cmd.OutOrStdout()
			rs := &recvState{
				messageID:       ingestResp.GetMessageId(),
				channelStreamID: cs.channelStreamID,
				out:             out,
				in:              cmd.InOrStdin(),
				isTTY:           isTerminal(out),
				toolCalls:       make(map[string]string),
			}

			return rs.recvLoop(ctx, cs.client, cs.stream)
		},
	}

	return cmd
}

var errTurnCancelled = errors.New("turn cancelled")

var errConnectionLost = errors.New("lost the connection to the relay before the reply finished; " +
	"the agent still has your message, run `syco chat` to read the reply")

// isTerminal reports whether w is the process's stdout attached to a
// terminal. Only then is output styled.
func isTerminal(w io.Writer) bool {
	f, ok := w.(*os.File)
	return ok && f == os.Stdout && isatty.IsTerminal(f.Fd())
}

// --- Stream helpers ---

// connectChannelStream dials the relay (or reuses conn), opens ChannelStreamReceive for
// the agent and reads the ack.
func connectChannelStream(ctx context.Context, server, agent string, conn *grpc.ClientConn) (connectedMsg, error) {
	if conn == nil {
		var err error
		conn, err = grpcclient.Dial(server)
		if err != nil {
			return connectedMsg{}, fmt.Errorf("connecting to relay: %w", err)
		}
	}
	client := relaypb.NewRelayGatewayClient(conn)
	stream, err := client.ChannelStreamReceive(ctx, &commonpb.ChannelStreamReceiveRequest{Agent: agent})
	if err != nil {
		conn.Close()
		return connectedMsg{}, fmt.Errorf("channel stream receive: %w", err)
	}
	channelStreamID, err := readAck(stream)
	if err != nil {
		conn.Close()
		return connectedMsg{}, fmt.Errorf("reading channel stream ack: %w", err)
	}
	return connectedMsg{conn: conn, client: client, stream: stream, channelStreamID: channelStreamID}, nil
}

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

type recvState struct {
	// messageID is the relay's id for this command's message: only the
	// frames of its turn are this command's.
	messageID string
	out       io.Writer
	isTTY     bool
	textBuf   strings.Builder
	toolCalls map[string]string // item_id -> tool name

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
) error {
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
		case line, ok := <-lines:
			rs.answerApproval(ctx, client, ok && approvesLine(line))
		case <-timer.C:
			log.Printf("dead connection detected (no frame in %v)", deadConnectionTimeout)
			return errConnectionLost
		case fe := <-frameCh:
			if fe.err != nil {
				if fe.err != io.EOF {
					log.Printf("stream error: %v", fe.err)
				}
				return errConnectionLost
			}
			frame := fe.frame
			timer.Reset(deadConnectionTimeout)

			if stop, err := rs.handleFrame(frame); stop {
				return err
			}
		}
	}
}

type frameOrErr struct {
	frame *commonpb.ChannelStreamOutbound
	err   error
}

func (rs *recvState) handleFrame(frame *commonpb.ChannelStreamOutbound) (bool, error) {
	switch c := frame.Command.(type) {
	case *commonpb.ChannelStreamOutbound_TurnState:
		if c.TurnState.GetMessageId() != rs.messageID {
			return false, nil
		}
		switch c.TurnState.State {
		case commonpb.TurnState_TURN_STATE_IDLE:
			flushText(rs.out, &rs.textBuf, rs.isTTY)
			return true, nil
		case commonpb.TurnState_TURN_STATE_FAILED:
			return true, fmt.Errorf("turn failed: %s", c.TurnState.Reason)
		case commonpb.TurnState_TURN_STATE_CANCELLED:
			return true, errTurnCancelled
		}

	case *commonpb.ChannelStreamOutbound_StreamItem:
		item := c.StreamItem
		if item.GetMessageId() != rs.messageID {
			return false, nil
		}
		rs.renderStreamItem(item)

	case *commonpb.ChannelStreamOutbound_ServerRequest:
		if a, ok := toolApprovalOf(c.ServerRequest); ok {
			rs.askApproval(a)
		}
	}

	return false, nil
}

// askApproval shows an approval request and queues it for a line of input.
func (rs *recvState) askApproval(a toolApproval) {
	rs.approvals = append(rs.approvals, a)
	if rs.lines == nil {
		rs.lines = readLines(rs.in)
	}
	rs.printToolLine(a.prompt())
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

func (rs *recvState) renderStreamItem(item *commonpb.StreamItem) {
	switch phase := item.Phase.(type) {
	case *commonpb.StreamItem_Start:
		if tu := phase.Start.GetToolUse(); tu != nil {
			rs.toolCalls[item.ItemId] = tu.Name
			rs.printToolLine(fmt.Sprintf("⚙ %s (started)", tu.Name))
		}
	case *commonpb.StreamItem_Delta:
		if delta := phase.Delta.GetTextDelta(); delta != "" {
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
		lipgloss.Fprintln(rs.out, toolStyle.Render(line))
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
