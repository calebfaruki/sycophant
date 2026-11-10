//go:build unix

package cmd

import (
	"fmt"
	"os"
	"syscall"
	"testing"
	"time"
)

// TestMain cuts the test binary off from any terminal before tests run.
// The chat command starts a full-screen program when it does not refuse
// first, and that program reads standard input or opens /dev/tty. With
// standard input on /dev/null and no controlling terminal, such a start
// fails at once with an error instead of taking over the developer's
// terminal or waiting for keys.
func TestMain(m *testing.M) {
	// Not UTC, so a test shows a time printed in local time.
	time.Local = time.FixedZone("UTC+5", 5*60*60)
	if devNull, err := os.Open(os.DevNull); err == nil {
		os.Stdin = devNull
	}
	if ttyReachable() {
		_, _ = syscall.Setsid()
	}
	if ttyReachable() {
		fmt.Fprintln(os.Stderr, "cmd tests: /dev/tty is still reachable after setsid; refusing to run tests that could start the full-screen program")
		os.Exit(2)
	}
	os.Exit(m.Run())
}

func ttyReachable() bool {
	f, err := os.Open("/dev/tty")
	if err != nil {
		return false
	}
	_ = f.Close()
	return true
}
