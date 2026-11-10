// The tests in this file cover the notices the TUI shows.

package cmd

import (
	"context"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/sycophant-md/sycophant/cmd/syco/internal/config"
	relaypb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/relay/v1"
	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

func TestTUILineUnderThePanelShowsTheReconnectReasonRedAndCutToTheScreenWidth(t *testing.T) {
	f, d, stream := connectedTUI(t)
	d.update(tea.WindowSizeMsg{Width: 80, Height: 24})
	d.send("UserNoticeDropPlover")
	d.fastRetries()
	reason := "GatewayDownPetrel " + strings.Repeat("y", 100)
	f.failReceives(1, reason)

	stream.end()
	d.waitView("the wait after the failed reconnect", time.Now().Add(5*time.Second), func(string) bool {
		return strings.Contains(d.footer(), "GatewayDownPetrel")
	})
	left := d.footerLeft()
	if !strings.HasPrefix(left, "disconnected · GatewayDownPetrel y") || !strings.HasSuffix(left, "… · reconnecting in 2s") {
		t.Fatalf("line under the panel is %q, want disconnected, the reason cut with …, then the countdown", left)
	}
	lines := strings.Split(d.view(), "\n")
	if w := ansi.StringWidth(lines[panelEdge(t, lines)+1]); w > 80 {
		t.Fatalf("line under the panel is %d wide, want at most the screen's 80", w)
	}
	red := strings.Split(errorStyle.Render("X"), "X")[0]
	if raw := d.rawLineWith("GatewayDownPetrel"); !strings.Contains(raw, red+"disconnected · GatewayDownPetrel") ||
		!strings.Contains(raw, statusStyle.Render(" · reconnecting in 2s")) {
		t.Fatalf("the reason is not red beside disconnected or the countdown not dim:\n%q", raw)
	}
}

func TestTUITurnFailedShowsInTheTranscriptAsOneRedLine(t *testing.T) {
	const msgID = "msg-notice-turns"
	_, d, s := tuiTurn(t, msgID)
	d.update(tea.WindowSizeMsg{Width: 120, Height: 30})
	s.push(failedTurnFrame(msgID, "TurnBrokeLoon\nTurnBrokeTail"))
	const failLine = "⚠ Turn failed: TurnBrokeLoon TurnBrokeTail"
	d.waitView("the failed turn", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, failLine)
	})
	if raw := d.rawLineWith(failLine); !strings.Contains(raw, errorStyle.Render(failLine)) {
		t.Fatalf("failed turn is not one red line:\n%q", raw)
	}
}

// The failed turn's streamed reply never reaches history, so the notice
// must stay under the message it followed, not move below the next one.
func TestTUITurnFailedStaysAboveTheNextMessageAfterItsTurnStarts(t *testing.T) {
	const msgID = "msg-notice-stays"
	_, d, s := tuiTurn(t, msgID)
	d.update(tea.WindowSizeMsg{Width: 120, Height: 30})
	s.push(streamTextFrames(msgID, "item-partial", "PartialReplyHeron")...)
	s.push(failedTurnFrame(msgID, "TurnBrokeKestrel"))
	const failLine = "⚠ Turn failed: TurnBrokeKestrel"
	d.waitView("the failed turn", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, failLine)
	})
	d.send("UserNextWren")
	d.startTurn(s, "msg-notice-next", "UserNextWren")
	lines := d.transcriptLines()
	fail, next := rowIndex(lines, failLine), rowIndex(lines, "UserNextWren")
	if fail < rowIndex(lines, "UserTurnLynx") || fail > next {
		t.Fatalf("after the next turn starts the notice is at row %d, want it between the message it followed (row %d) and the next message (row %d):\n%s",
			fail, rowIndex(lines, "UserTurnLynx"), next, d.transcript())
	}
}

func TestTUITurnFailedStaysShownAfterAReconnect(t *testing.T) {
	const msgID = "msg-notice-reconnect"
	_, d, s := tuiTurn(t, msgID)
	d.update(tea.WindowSizeMsg{Width: 120, Height: 30})
	s.push(failedTurnFrame(msgID, "TurnBrokeGrebe"))
	const failLine = "⚠ Turn failed: TurnBrokeGrebe"
	d.waitView("the failed turn", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, failLine)
	})
	d.dropAndReconnect(s)
	if !strings.Contains(d.transcript(), failLine) {
		t.Fatalf("notice gone after the reconnect:\n%s", d.transcript())
	}
}

// Another client's clear leaves the history shorter than the transcript
// when the next turn starts, so the notice goes after every message.
func TestTUITurnFailedStaysShownAfterTheHistoryShrinks(t *testing.T) {
	const msgID = "msg-notice-shrink"
	f, d, s := tuiTurn(t, msgID)
	d.update(tea.WindowSizeMsg{Width: 120, Height: 30})
	s.push(failedTurnFrame(msgID, "TurnBrokeIbis"))
	const failLine = "⚠ Turn failed: TurnBrokeIbis"
	d.waitView("the failed turn", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, failLine)
	})
	f.setHistory(userEntry("UserAfterClearIbis"))
	d.startTurn(s, "msg-notice-shrink-2", "UserAfterClearIbis")
	lines := d.transcriptLines()
	if rowIndex(lines, failLine) <= rowIndex(lines, "UserAfterClearIbis") {
		t.Fatalf("notice not after the message once the history shrank:\n%s", d.transcript())
	}
}

// downClient fails every call these tests make with one gRPC status.
type downClient struct {
	relaypb.RelayGatewayClient
	err error
}

func (c downClient) TakeBackMessage(context.Context, *commonpb.TakeBackMessageRequest, ...grpc.CallOption) (*commonpb.TakeBackMessageResponse, error) {
	return nil, c.err
}

func (c downClient) ChannelStreamIngest(context.Context, *commonpb.ChannelStreamIngestRequest, ...grpc.CallOption) (*commonpb.ChannelStreamIngestAck, error) {
	return nil, c.err
}

func (c downClient) GetConversationHistory(context.Context, *commonpb.GetConversationHistoryRequest, ...grpc.CallOption) (*commonpb.GetConversationHistoryResponse, error) {
	return nil, c.err
}

func TestTUIFailedCallNoticesShowTheActionAndTheRelaysReason(t *testing.T) {
	_, d, _ := connectedTUI(t)
	m := d.model.(*tuiModel)
	m.started = true
	m.client = downClient{err: status.Error(codes.Unavailable, "RelayDownMarten")}
	for _, tc := range []struct {
		cmd  tea.Cmd
		want string
	}{
		{m.takeBackCmd(), "⚠ taking back a message: RelayDownMarten"},
		{m.answerApprovalCmd("req-1", true), "⚠ sending approval answer: RelayDownMarten"},
		{m.historyCmd(m.reloadGen), "⚠ loading the conversation of " + testAgent + ": RelayDownMarten"},
	} {
		d.update(tc.cmd())
		if !strings.Contains(d.transcript(), tc.want) {
			t.Errorf("transcript lacks %q:\n%s", tc.want, d.transcript())
		}
	}
}

// The fake gateway has no ListAgents, so the picker's call fails.
func TestTUIPickerShowsTheRelaysReasonForAFailedAgentList(t *testing.T) {
	newClientEnv(t)
	cfgDir, p, token := testProfile(t)
	m := newTUIModel(cfgDir, &config.Profile{Name: p.Name, Server: p.Server}, token)
	msg := m.listAgentsCmd()()
	d := newTUIDriver(t, m)
	d.update(tea.WindowSizeMsg{Width: 160, Height: 40})
	d.update(msg)
	if want := "⚠ listing agents: method ListAgents not implemented"; !strings.Contains(d.view(), want) {
		t.Fatalf("picker view lacks %q:\n%s", want, d.view())
	}
}
