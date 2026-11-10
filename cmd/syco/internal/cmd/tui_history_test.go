// The tests in this file cover recalling sent messages.

package cmd

import (
	"os"
	"path/filepath"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/sycophant-md/sycophant/cmd/syco/internal/config"
)

func TestTUIUpAndDownBrowseSentMessagesAndKeepTheDraft(t *testing.T) {
	_, d, _ := connectedTUI(t)
	d.send("OldestCrane")
	d.send("MiddleEgret")
	d.send("NewestStork")

	d.update(keyText("DraftPelican"))
	d.update(keyPress(tea.KeyUp))
	d.wantInput("one Up", "NewestStork")
	d.update(keyPress(tea.KeyUp))
	d.update(keyPress(tea.KeyUp))
	d.wantInput("three Ups", "OldestCrane")
	d.update(keyPress(tea.KeyUp))
	d.wantInput("Up past the oldest", "OldestCrane")

	d.update(keyPress(tea.KeyDown))
	d.wantInput("Down", "MiddleEgret")
	d.update(keyPress(tea.KeyDown))
	d.update(keyPress(tea.KeyDown))
	d.wantInput("Down past the newest", "DraftPelican")
	d.update(keyPress(tea.KeyDown))
	d.wantInput("another Down", "DraftPelican")
}

func TestTUIUpAndDownMoveBetweenInputLinesAndBrowseOnlyFromTheEnds(t *testing.T) {
	_, d, _ := connectedTUI(t)
	d.send("EarlierWren")

	d.typeLines("DraftTopFinch", "DraftBottomRobin")
	d.update(keyPress(tea.KeyUp))
	d.wantInput("Up from the bottom line", "DraftTopFinch\nDraftBottomRobin")
	d.update(keyPress(tea.KeyUp))
	d.wantInput("Up from the top line", "EarlierWren")

	d.update(keyPress(tea.KeyDown))
	d.wantInput("Down past the newest", "DraftTopFinch\nDraftBottomRobin")

	// A recalled multi-line message: Down from its top line moves the
	// cursor; Down from its bottom line browses forward.
	d.enter() // Enter sends the restored draft.
	d.update(keyPress(tea.KeyUp))
	d.wantInput("Up on an empty input", "DraftTopFinch\nDraftBottomRobin")
	d.update(keyPress(tea.KeyDown))
	d.wantInput("Down from the recalled message's top line", "DraftTopFinch\nDraftBottomRobin")
	d.update(keyPress(tea.KeyDown))
	d.wantInput("Down from its bottom line", "")
}

func TestTUISentMessagesAreRecalledAfterARestartWithTheirLineBreaks(t *testing.T) {
	f, d, _ := connectedTUI(t)
	d.typeLines("RestartLineOneBadger", "RestartLineTwoMole")
	d.enter()

	d2 := openTUI(t, f)
	f.nextStream(t, 5*time.Second)
	d2.waitConnected()
	d2.update(keyPress(tea.KeyUp))
	d2.wantInput("Up in a new TUI", "RestartLineOneBadger\nRestartLineTwoMole")
}

func TestTUIOpensAndSendsWhenTheHistoryFileIsUnreadable(t *testing.T) {
	f, dir := newClientEnv(t)
	if err := os.Mkdir(filepath.Join(dir, config.InputHistoryFile), 0o755); err != nil {
		t.Fatal(err)
	}
	d := openTUI(t, f)
	f.nextStream(t, 5*time.Second)
	d.waitConnected()
	d.send("UnreadableHistoryShrew")
	d.update(keyPress(tea.KeyUp))
	d.wantInput("Up after sending", "UnreadableHistoryShrew")
}
