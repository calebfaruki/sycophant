// The tests in this file cover how the TUI draws who sent each message,
// and the reset a clear or compact from another device brings.

package cmd

import (
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"

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

func TestTUIHistoryShowsEachSpeakersNameAndKindAboveTheirMessageOnABarInTheirColour(t *testing.T) {
	f, _ := newClientEnv(t)
	f.setHistory(
		sentBy(personSender("fdsa"), "FromPersonSpeakerAuk"),
		assistantEntry("FromMainSpeakerAuk"),
		sentBy(&commonpb.Sender{From: &commonpb.Sender_SubAgent{SubAgent: "researcher/7f3a91"}}, "FromSubAgentSpeakerAuk"),
	)
	d := openTUI(t, f)
	f.nextStream(t, 5*time.Second)
	d.waitView("the three messages", time.Now().Add(5*time.Second), func(string) bool {
		return strings.Contains(d.transcript(), "FromSubAgentSpeakerAuk")
	})
	lines := d.transcriptLines()
	for _, c := range []struct{ text, speaker, bar string }{
		{"FromPersonSpeakerAuk", "fdsa · person", panelBar(brandCyan)},
		{"FromMainSpeakerAuk", testAgent + " · main", panelBar(brandPink)},
		{"FromSubAgentSpeakerAuk", "researcher · sub-agent", panelBar(brandPink)},
	} {
		if got := speakerAbove(lines, c.text); got != c.speaker {
			t.Errorf("above %s is %q, want %q:\n%s", c.text, got, c.speaker, d.transcript())
		}
		if !strings.Contains(d.rawLineWith(c.text), c.bar) {
			t.Errorf("%s does not have its bar in its colour:\n%q", c.text, d.rawLineWith(c.text))
		}
	}
}

func TestTUILiveReplyShowsTheMainAgentsNameAboveItOnAPinkBarWhileStreaming(t *testing.T) {
	const msgID = "msg-live-speaker"
	_, d, s := tuiTurn(t, msgID)
	d.update(tea.WindowSizeMsg{Width: 80, Height: 24})
	s.push(streamTextFrames(msgID, "item-1", "LiveReplySpeakerAuk")...)
	d.waitView("the streamed reply", time.Now().Add(5*time.Second), func(v string) bool {
		return strings.Contains(v, "LiveReplySpeakerAuk")
	})
	if got := speakerAbove(d.transcriptLines(), "LiveReplySpeakerAuk"); got != testAgent+" · main" {
		t.Fatalf("above the reply is %q, want %q:\n%s", got, testAgent+" · main", d.transcript())
	}
	if !strings.Contains(d.rawLineWith("LiveReplySpeakerAuk"), panelBar(brandPink)) {
		t.Fatalf("the reply's bar is not pink:\n%q", d.rawLineWith("LiveReplySpeakerAuk"))
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

func TestTUIHistoryShowsAPersonsDisplayName(t *testing.T) {
	f, _ := newClientEnv(t)
	named := personSender("https://issuer.example#alice")
	name := "Alice Liddell"
	named.DisplayName = &name
	f.setHistory(sentBy(named, "FromNamedPersonTern"))
	d := openTUI(t, f)
	f.nextStream(t, 5*time.Second)
	d.waitView("the message", time.Now().Add(5*time.Second), func(string) bool {
		return strings.Contains(d.transcript(), "FromNamedPersonTern")
	})
	if got, want := speakerAbove(d.transcriptLines(), "FromNamedPersonTern"), "Alice Liddell · person"; got != want {
		t.Errorf("above FromNamedPersonTern is %q, want %q:\n%s", got, want, d.transcript())
	}
}

func TestTUIShowsControlCharactersInANameAsCaretsOnOneLine(t *testing.T) {
	f, _ := newClientEnv(t)
	sender := personSender("https://issuer.example#mallory")
	name := "Mal\x1b[2Jlory\nTail\x7f\u0085"
	sender.DisplayName = &name
	f.setHistory(sentBy(sender, "FromControlNameShrike"))
	d := openTUI(t, f)
	f.nextStream(t, 5*time.Second)
	d.waitView("the message", time.Now().Add(5*time.Second), func(string) bool {
		return strings.Contains(d.transcript(), "FromControlNameShrike")
	})
	if got, want := speakerAbove(d.transcriptLines(), "FromControlNameShrike"), `Mal^[[2Jlory^JTail^?\u0085 · person`; got != want {
		t.Fatalf("above the message is %q, want %q:\n%s", got, want, d.transcript())
	}
}

func TestTUIFooterShowsTheRelaysDisplayNameAndTheTokenIdentityWhenNoneIsSet(t *testing.T) {
	for _, tc := range []struct{ name, displayName, want string }{
		{"named", "Caleb Faruki", "Caleb Faruki"},
		{"unnamed", "", "tester"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f, _ := newClientEnv(t)
			f.setAgentsReply(&commonpb.ListAgentsResponse{DisplayName: tc.displayName})
			d := openTUI(t, f)
			f.nextStream(t, 5*time.Second)
			d.waitConnected()
			if got, want := d.footerLeft(), tc.want+" · "+f.addr; got != want {
				t.Fatalf("the footer shows %q, want %q", got, want)
			}
		})
	}
}

func TestTUIPickerShowsTheRelaysDisplayName(t *testing.T) {
	newClientEnv(t)
	cfgDir, p, token := testProfile(t)
	p.Agent = ""
	d := newTUIDriver(t, newTUIModel(cfgDir, p, token))
	d.update(tea.WindowSizeMsg{Width: 80, Height: 24})
	d.update(agentsMsg{agents: []string{"one", "two"}, displayName: "Caleb Faruki"})
	if want := "Caleb Faruki · " + p.Server; !strings.Contains(d.view(), want) {
		t.Fatalf("the picker does not show %q:\n%s", want, d.view())
	}
}
