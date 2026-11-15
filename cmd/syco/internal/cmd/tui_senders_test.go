// The tests in this file cover how the TUI draws who sent each message,
// and the reset a clear or compact from another device brings.

package cmd

import (
	"strings"
	"testing"
	"time"

	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

// drawnAs is the block of rows that draws text, from the blank row above it
// to the blank row below, with text itself taken out.
func drawnAs(lines []string, text string) string {
	at := rowIndex(lines, text)
	if at < 0 {
		return ""
	}
	first, last := at, at
	for first > 0 && strings.TrimSpace(lines[first-1]) != "" {
		first--
	}
	for last < len(lines)-1 && strings.TrimSpace(lines[last+1]) != "" {
		last++
	}
	rows := make([]string, 0, last-first+1)
	for _, l := range lines[first : last+1] {
		rows = append(rows, strings.TrimRight(strings.Replace(l, text, "<text>", 1), " "))
	}
	return strings.Join(rows, "\n")
}

// The four texts are one length, so their blocks differ only in how each
// sender is drawn.
func TestTUIHistoryDrawsAPersonASubAgentAParentAgentAndSycophantEachDifferently(t *testing.T) {
	f, _ := newClientEnv(t)
	messages := []struct {
		text   string
		sender *commonpb.Sender
	}{
		{"FromPersonGannet", personSender("tester")},
		{"FromSubAgtGannet", &commonpb.Sender{From: &commonpb.Sender_SubAgent{SubAgent: "scout/SenderPathGannet"}}},
		{"FromParentGannet", parentAgentSender(testAgent)},
		{"FromSycophGannet", &commonpb.Sender{From: &commonpb.Sender_Sycophant{Sycophant: true}}},
	}
	var entries []*commonpb.HistoryEntry
	for _, m := range messages {
		entries = append(entries, sentBy(m.sender, m.text))
	}
	f.setHistory(entries...)
	d := openTUI(t, f)
	f.nextStream(t, 5*time.Second)
	d.waitView("the four messages", time.Now().Add(5*time.Second), func(string) bool {
		tr := d.transcript()
		for _, m := range messages {
			if !strings.Contains(tr, m.text) {
				return false
			}
		}
		return true
	})

	lines := d.transcriptLines()
	seen := map[string]string{}
	for _, m := range messages {
		block := drawnAs(lines, m.text)
		if other, ok := seen[block]; ok {
			t.Fatalf("%s and %s are drawn the same way:\n%s\ntranscript:\n%s", other, m.text, block, d.transcript())
		}
		seen[block] = m.text
	}
}

// The relay's history after the clear or compact holds only its entry, which
// is drawn as nothing.
func TestTUIClearOrCompactFromAnotherDeviceResetsTheScreenAndHidesTheCompactSummary(t *testing.T) {
	for _, tc := range []struct {
		name    string
		kind    commonpb.EntryKind
		content string
	}{
		{"clear", commonpb.EntryKind_ENTRY_KIND_CLEAR, ""},
		{"compact", commonpb.EntryKind_ENTRY_KIND_COMPACT, "CompactSummaryJacana"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f, d, _ := startedTUI(t, "UserBeforeResetJacana")
			e := sentBy(personSender("other-device-person"), tc.content)
			e.Kind = tc.kind
			f.setHistory(e)
			f.sendEntry(tc.kind)
			d.waitView("the screen reset", time.Now().Add(5*time.Second), func(string) bool {
				return !strings.Contains(d.transcript(), "UserBeforeResetJacana")
			})
			if tc.content != "" && strings.Contains(d.transcript(), tc.content) {
				t.Fatalf("the %s entry is drawn:\n%s", tc.name, d.transcript())
			}
		})
	}
}
