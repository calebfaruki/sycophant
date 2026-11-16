package cmd

import (
	"bytes"
	_ "embed"
	"encoding/base64"
	"fmt"
	"image"
	"image/png"
	"io"
	"regexp"
	"strconv"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
)

//go:embed mascot.png
var mascotPNG []byte

const (
	startMascotID = 42
	chatMascotID  = 43
)

var diacritics = []rune{
	0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F,
	0x0346, 0x034A, 0x034B, 0x034C, 0x0350, 0x0351, 0x0352, 0x0357,
	0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
	0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484,
}

var (
	attributesReplyRe = regexp.MustCompile(`\x1b\[\?[0-9;]*c`)
	cellSizeReplyRe   = regexp.MustCompile(`\x1b\[6;(\d+);(\d+)t`)
)

func probeImages(in io.Reader, out io.Writer, wait time.Duration) (cellW, cellH int, ok bool) {
	if _, err := io.WriteString(out, "\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[c"); err != nil {
		return 0, 0, false
	}
	got := make(chan string, 1)
	go func() {
		var buf []byte
		b := make([]byte, 256)
		for {
			n, err := in.Read(b)
			buf = append(buf, b[:n]...)
			if loc := attributesReplyRe.FindIndex(buf); loc != nil {
				got <- string(buf[:loc[0]])
				return
			}
			if err != nil {
				close(got)
				return
			}
		}
	}()
	var reply string
	select {
	case r, done := <-got:
		if !done {
			return 0, 0, false
		}
		reply = r
	case <-time.After(wait):
		return 0, 0, false
	}
	if !strings.Contains(reply, "\x1b_Gi=31;OK") {
		return 0, 0, false
	}
	m := cellSizeReplyRe.FindStringSubmatch(reply)
	if m == nil {
		return 0, 0, false
	}
	cellH, _ = strconv.Atoi(m[1])
	cellW, _ = strconv.Atoi(m[2])
	if cellW <= 0 || cellH <= 0 {
		return 0, 0, false
	}
	return cellW, cellH, true
}

type mascot struct {
	cols, rows int
	upload     string
	art        string
}

func newMascot(sprite image.Image, id, size, cellW, cellH, lift int) mascot {
	sb := sprite.Bounds()
	sw, sh := sb.Dx()*size/sb.Dy(), size
	feet := ((sb.Dy()-bottomGap(sprite))*size + sb.Dy() - 1) / sb.Dy()
	cols, rows := (sw+cellW-1)/cellW, (feet+lift+cellH-1)/cellH
	canvas := image.NewNRGBA(image.Rect(0, 0, cols*cellW, rows*cellH))
	ox, oy := (cols*cellW-sw)/2, rows*cellH-lift-feet
	for y := 0; y < sh; y++ {
		for x := 0; x < sw; x++ {
			canvas.Set(ox+x, oy+y, sprite.At(sb.Min.X+x*sb.Dx()/sw, sb.Min.Y+y*sb.Dy()/sh))
		}
	}

	var buf bytes.Buffer
	_ = png.Encode(&buf, canvas)
	enc := base64.StdEncoding.EncodeToString(buf.Bytes())
	var up strings.Builder
	for i := 0; i < len(enc); i += 4096 {
		chunk := enc[i:min(i+4096, len(enc))]
		more := 1
		if i+4096 >= len(enc) {
			more = 0
		}
		if i == 0 {
			fmt.Fprintf(&up, "\x1b_Ga=T,U=1,q=2,f=100,i=%d,c=%d,r=%d,m=%d;%s\x1b\\", id, cols, rows, more, chunk)
		} else {
			fmt.Fprintf(&up, "\x1b_Gm=%d;%s\x1b\\", more, chunk)
		}
	}

	style := lipgloss.NewStyle().Foreground(lipgloss.Color(strconv.Itoa(id)))
	lines := make([]string, rows)
	for r := range lines {
		var row strings.Builder
		for c := 0; c < cols; c++ {
			row.WriteRune(0x10EEEE)
			if c == 0 && r < len(diacritics) {
				row.WriteRune(diacritics[r])
			}
		}
		lines[r] = style.Render(row.String())
	}

	return mascot{cols: cols, rows: rows, upload: up.String(), art: strings.Join(lines, "\n")}
}

func bottomGap(sprite image.Image) int {
	b := sprite.Bounds()
	for y := b.Max.Y - 1; y >= b.Min.Y; y-- {
		for x := b.Min.X; x < b.Max.X; x++ {
			if _, _, _, a := sprite.At(x, y).RGBA(); a != 0 {
				return b.Max.Y - 1 - y
			}
		}
	}
	return 0
}
