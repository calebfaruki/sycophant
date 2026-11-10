package cmd

import (
	"fmt"

	"github.com/spf13/cobra"

	"github.com/sycophant-md/sycophant/cmd/syco/internal/config"
)

func newLogoutCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "logout",
		Short: "Remove saved credentials for the active profile",
		RunE: func(cmd *cobra.Command, args []string) error {
			cfgDir := config.ConfigDir()
			profileFlag, _ := cmd.Flags().GetString("profile")
			p, err := config.LoadProfile(cfgDir, profileFlag)
			if err != nil {
				return err
			}
			if err := config.DeleteToken(cfgDir, p.Name); err != nil {
				return err
			}
			fmt.Fprintln(cmd.ErrOrStderr(), "Logged out.")
			return nil
		},
	}
}
