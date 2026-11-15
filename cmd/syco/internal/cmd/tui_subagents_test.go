// The tests in this file cover the sub-agent commands and the sub-agent
// events the TUI shows.

package cmd

import (
	"strings"
	"testing"
	"time"

	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

func TestTUISubagentsCommandListsTheAgentsSubAgents(t *testing.T) {
	f, d, _ := startedTUI(t, "UserBeforeListPuffin")
	f.setSubAgents("scout/ListPathAvocet", "critic/ListPathBrant")
	d.typeEnter("/subagents")

	req, ok := f.nextSubAgentCall(t, 5*time.Second).(*commonpb.ListSubAgentsRequest)
	if !ok {
		t.Fatalf("/subagents sent no ListSubAgents")
	}
	if req.GetAgent() != testAgent {
		t.Fatalf("/subagents listed agent %q, want %q", req.GetAgent(), testAgent)
	}
	d.waitView("the listed sub-agents", time.Now().Add(5*time.Second), func(string) bool {
		tr := d.transcript()
		return strings.Contains(tr, "scout/ListPathAvocet") && strings.Contains(tr, "critic/ListPathBrant")
	})
	if n := len(f.ingestLog()); n != 0 {
		t.Fatalf("/subagents sent %d chat messages, want none", n)
	}
}

func TestTUILogCommandShowsTheSubAgentsConversation(t *testing.T) {
	f, d, _ := startedTUI(t, "UserBeforeLogPuffin")
	f.setSubAgentLog(sentBy(parentAgentSender(testAgent), "LogTaskBittern"), assistantEntry("LogAnswerBittern"))
	d.typeEnter("/log scout/LogPathCurlew")

	req, ok := f.nextSubAgentCall(t, 5*time.Second).(*commonpb.ReadSubAgentLogRequest)
	if !ok {
		t.Fatalf("/log sent no ReadSubAgentLog")
	}
	if req.GetAgent() != testAgent || req.GetPath() != "scout/LogPathCurlew" {
		t.Fatalf("/log read agent %q path %q, want %q and scout/LogPathCurlew", req.GetAgent(), req.GetPath(), testAgent)
	}
	d.waitView("the sub-agent's conversation", time.Now().Add(5*time.Second), func(string) bool {
		tr := d.transcript()
		return strings.Contains(tr, "LogTaskBittern") && strings.Contains(tr, "LogAnswerBittern")
	})
	if !strings.Contains(d.transcript(), "parent agent: LogTaskBittern") {
		t.Fatalf("the mandate row does not read parent agent: LogTaskBittern:\n%s", d.transcript())
	}
	if n := len(f.ingestLog()); n != 0 {
		t.Fatalf("/log sent %d chat messages, want none", n)
	}
}

func TestTUIMsgCommandSendsTheTextToTheSubAgent(t *testing.T) {
	f, d, _ := connectedTUI(t)
	d.typeEnter("/msg scout/MsgPathDunlin look at MsgTextEider again")

	req, ok := f.nextSubAgentCall(t, 5*time.Second).(*commonpb.MessageSubAgentRequest)
	if !ok {
		t.Fatalf("/msg sent no MessageSubAgent")
	}
	if req.GetAgent() != testAgent || req.GetPath() != "scout/MsgPathDunlin" || req.GetText() != "look at MsgTextEider again" {
		t.Fatalf("/msg sent agent %q path %q text %q, want %q, scout/MsgPathDunlin and the rest of the line",
			req.GetAgent(), req.GetPath(), req.GetText(), testAgent)
	}
	if n := len(f.ingestLog()); n != 0 {
		t.Fatalf("/msg sent %d chat messages, want none", n)
	}
}

func TestTUIStopCommandStopsTheSubAgent(t *testing.T) {
	f, d, _ := connectedTUI(t)
	d.typeEnter("/stop scout/StopPathFulmar")

	req, ok := f.nextSubAgentCall(t, 5*time.Second).(*commonpb.StopSubAgentRequest)
	if !ok {
		t.Fatalf("/stop sent no StopSubAgent")
	}
	if req.GetAgent() != testAgent || req.GetPath() != "scout/StopPathFulmar" {
		t.Fatalf("/stop stopped agent %q path %q, want %q and scout/StopPathFulmar", req.GetAgent(), req.GetPath(), testAgent)
	}
	if n := len(f.ingestLog()); n != 0 {
		t.Fatalf("/stop sent %d chat messages, want none", n)
	}
}

// With the path taken out, the started and stopped rows still differ only
// if each says what happened.
func TestTUISubAgentEventsShowTheirPathAnswerOrFailureAndStartedReadsUnlikeStopped(t *testing.T) {
	_, d, s := startedTUI(t, "UserBeforeEventsPuffin")
	s.push(
		&commonpb.ChannelStreamOutbound{Command: &commonpb.ChannelStreamOutbound_SubAgentStarted{
			SubAgentStarted: &commonpb.SubAgentStarted{Path: "scout/EventStartGrebe"},
		}},
		&commonpb.ChannelStreamOutbound{Command: &commonpb.ChannelStreamOutbound_SubAgentFinished{
			SubAgentFinished: &commonpb.SubAgentFinished{Path: "scout/EventDoneeGrebe", Outcome: &commonpb.SubAgentFinished_Answer{Answer: "AnswerTextHeron"}},
		}},
		&commonpb.ChannelStreamOutbound{Command: &commonpb.ChannelStreamOutbound_SubAgentFinished{
			SubAgentFinished: &commonpb.SubAgentFinished{Path: "scout/EventFailsGrebe", Outcome: &commonpb.SubAgentFinished_Failure{Failure: "FailureTextIbis"}},
		}},
		&commonpb.ChannelStreamOutbound{Command: &commonpb.ChannelStreamOutbound_SubAgentStopped{
			SubAgentStopped: &commonpb.SubAgentStopped{Path: "scout/EventStopsGrebe"},
		}},
	)
	want := []string{
		"scout/EventStartGrebe",
		"scout/EventDoneeGrebe", "AnswerTextHeron",
		"scout/EventFailsGrebe", "FailureTextIbis",
		"scout/EventStopsGrebe",
	}
	d.waitView("the four sub-agent notices", time.Now().Add(5*time.Second), func(string) bool {
		tr := d.transcript()
		for _, w := range want {
			if !strings.Contains(tr, w) {
				return false
			}
		}
		return true
	})
	lines := d.transcriptLines()
	started := strings.TrimSpace(strings.Replace(lines[rowIndex(lines, "scout/EventStartGrebe")], "scout/EventStartGrebe", "<path>", 1))
	stopped := strings.TrimSpace(strings.Replace(lines[rowIndex(lines, "scout/EventStopsGrebe")], "scout/EventStopsGrebe", "<path>", 1))
	if started == stopped {
		t.Fatalf("the started and stopped notices read the same, %q:\n%s", started, d.transcript())
	}
}
