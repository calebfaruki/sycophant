// Package cmd implements the syco CLI commands.
package cmd

import (
	"io"
	"os"
	"unicode/utf8"

	"github.com/spf13/cobra"
)

const version = "0.1.0"

type caretWriter struct {
	io.Writer
	held []byte
}

// Write holds back a character split across writes until the rest arrives.
func (w *caretWriter) Write(p []byte) (int, error) {
	b := append(w.held, p...)
	cut := len(b)
	for i := len(b) - 1; i >= 0 && i > len(b)-utf8.UTFMax; i-- {
		if utf8.RuneStart(b[i]) {
			if !utf8.FullRune(b[i:]) {
				cut = i
			}
			break
		}
	}
	w.held = append([]byte(nil), b[cut:]...)
	_, err := io.WriteString(w.Writer, caretControls(string(b[:cut]), keepInBody))
	return len(p), err
}

// Root returns the top-level cobra command for the syco CLI.
func Root() *cobra.Command {
	root := &cobra.Command{
		Use:          "syco",
		Short:        "Sycophant end-user CLI",
		SilenceUsage: true,
	}
	root.SetErr(&caretWriter{Writer: os.Stderr})

	root.PersistentFlags().String("profile", "", "Profile to use (overrides SYCO_PROFILE and [default].profile)")

	root.AddCommand(
		newLoginCmd(),
		newLogoutCmd(),
		newWhoamiCmd(),
		newSessionCmd(),
		newProfileCmd(),
		newChatCmd(),
	)

	return root
}
