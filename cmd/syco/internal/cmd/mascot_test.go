// The tests in this file cover the terminal's image check and the mascot
// images sent to it.

package cmd

import (
	"bytes"
	"encoding/base64"
	"image/color"
	"image/png"
	"io"
	"os"
	"regexp"
	"strconv"
	"strings"
	"testing"
	"time"

	"charm.land/lipgloss/v2"

	"github.com/sycophant-md/sycophant/cmd/syco/internal/config"
)

const (
	graphicsReply   = "\x1b_Gi=31;OK\x1b\\"
	cellSizeReply   = "\x1b[6;16;8t"
	attributesReply = "\x1b[?62;22c"
)

func TestProbeImagesAsksForGraphicsBeforeAttributesAndOnlyReportsImagesWhenAGraphicsReplyAndCellSizeComeFirst(t *testing.T) {
	cases := []struct {
		name  string
		reply string
		want  bool
	}{
		{"graphics reply and cell size before attributes", graphicsReply + cellSizeReply + attributesReply, true},
		{"attributes only", attributesReply, false},
		{"graphics reply without cell size", graphicsReply + attributesReply, false},
		{"graphics reply and cell size after attributes", attributesReply + graphicsReply + cellSizeReply, false},
		{"cell size without graphics reply", cellSizeReply + attributesReply, false},
	}
	for _, c := range cases {
		var out bytes.Buffer
		cellW, cellH, ok := probeImages(strings.NewReader(c.reply), &out, 200*time.Millisecond)
		if ok != c.want {
			t.Fatalf("%s: images reported %v, want %v", c.name, ok, c.want)
		}
		if c.want && (cellW != 8 || cellH != 16) {
			t.Fatalf("%s: cell size %dx%d, want 8x16 from %q", c.name, cellW, cellH, cellSizeReply)
		}
		q := out.String()
		g, a := strings.Index(q, "\x1b_G"), strings.Index(q, "\x1b[c")
		if g < 0 || a < 0 || g > a || !strings.Contains(q[g:a], "a=q") {
			t.Fatalf("%s: wrote %q, want a graphics query before the attributes query", c.name, q)
		}
	}
}

func TestProbeImagesReportsNoImagesWithin200msWhenTheTerminalStaysSilent(t *testing.T) {
	in, w := io.Pipe()
	t.Cleanup(func() { w.Close() })
	done := make(chan bool, 1)
	start := time.Now()
	go func() {
		_, _, ok := probeImages(in, io.Discard, 200*time.Millisecond)
		done <- ok
	}()
	select {
	case ok := <-done:
		if ok {
			t.Fatal("images reported on a silent terminal")
		}
		if elapsed := time.Since(start); elapsed > 300*time.Millisecond {
			t.Fatalf("check gave up after %v, want about 200ms", elapsed)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("check still waiting on a silent terminal after 2s")
	}
}

var graphicsChunkRe = regexp.MustCompile("\x1b_G([^;\x1b]*);([^\x1b]*)\x1b\\\\")

func TestMascotImagesAreTheSpriteScaledToTheirSizeAtTheBottomCentredAndPaddedToWholeCells(t *testing.T) {
	f, err := os.Open("mascot.png")
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	sprite, err := png.Decode(f)
	if err != nil {
		t.Fatal(err)
	}

	const cellW, cellH = 7, 15
	m := newTUIModel(t.TempDir(), &config.Profile{Server: "mascot.invalid:1", Agent: "a-mascot"}, "")
	m.enableImages(cellW, cellH)
	sb := sprite.Bounds()
	clearRow := func(y int) bool {
		for x := sb.Min.X; x < sb.Max.X; x++ {
			if _, _, _, a := sprite.At(x, y).RGBA(); a != 0 {
				return false
			}
		}
		return true
	}
	// gap is the sprite's clear rows under its feet.
	gap := 0
	for y := sb.Max.Y - 1; y >= sb.Min.Y && clearRow(y); y-- {
		gap++
	}
	for _, c := range []struct {
		name string
		// feet is how many pixels the sprite stands tall, from its top to
		// its feet, and lift how many pixels its feet sit above the
		// image's bottom edge.
		feet, lift int
		m          mascot
	}{
		{"start screen", 6 * (32 - gap), cellH/2 - 6, m.startMascot},
		{"chat, as tall as the panel around a one-line input", panelChrome * cellH, 0, m.chatMascot},
	} {
		size := c.feet * 32 / (32 - gap)
		cols, rows := (size+cellW-1)/cellW, (c.feet+c.lift+cellH-1)/cellH

		chunks := graphicsChunkRe.FindAllStringSubmatch(c.m.upload, -1)
		if len(chunks) == 0 {
			t.Fatalf("%s: upload %q holds no graphics command", c.name, c.m.upload)
		}
		keys := map[string]string{}
		for _, kv := range strings.Split(chunks[0][1], ",") {
			k, v, _ := strings.Cut(kv, "=")
			keys[k] = v
		}
		if keys["c"] != strconv.Itoa(cols) || keys["r"] != strconv.Itoa(rows) {
			t.Fatalf("%s: image placed at c=%s r=%s, want %d columns and %d rows", c.name, keys["c"], keys["r"], cols, rows)
		}
		if w, h := lipgloss.Width(c.m.art), lipgloss.Height(c.m.art); w != cols || h != rows {
			t.Fatalf("%s: placeholder is %dx%d cells, want %dx%d", c.name, w, h, cols, rows)
		}

		var payload strings.Builder
		for _, ch := range chunks {
			payload.WriteString(ch[2])
		}
		data, err := base64.StdEncoding.DecodeString(payload.String())
		if err != nil {
			t.Fatalf("%s: payload is not base64: %v", c.name, err)
		}
		img, err := png.Decode(bytes.NewReader(data))
		if err != nil {
			t.Fatalf("%s: payload is not a PNG: %v", c.name, err)
		}
		b := img.Bounds()
		W, H := b.Dx(), b.Dy()
		if W != cols*cellW || H != rows*cellH {
			t.Fatalf("%s: image is %dx%d px, want %dx%d (whole %dx%d cells)", c.name, W, H, cols*cellW, rows*cellH, cellW, cellH)
		}
		ox, oy := (W-size)/2, H-c.lift-c.feet
		for y := 0; y < H; y++ {
			for x := 0; x < W; x++ {
				got := color.NRGBAModel.Convert(img.At(b.Min.X+x, b.Min.Y+y)).(color.NRGBA)
				if x < ox || x >= ox+size || y < oy || y >= oy+size {
					if got.A != 0 {
						t.Fatalf("%s: padding pixel (%d,%d) is %v, want transparent", c.name, x, y, got)
					}
					continue
				}
				sx, sy := (x-ox)*32/size, (y-oy)*32/size
				want := color.NRGBAModel.Convert(sprite.At(sb.Min.X+sx, sb.Min.Y+sy)).(color.NRGBA)
				if got != want && (got.A != 0 || want.A != 0) {
					t.Fatalf("%s: pixel (%d,%d) is %v, want sprite pixel (%d,%d) %v", c.name, x, y, got, sx, sy, want)
				}
			}
		}
	}
}
