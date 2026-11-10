package cmd

import (
	"fmt"

	"github.com/spf13/cobra"

	"github.com/sycophant-md/sycophant/cmd/syco/internal/config"
)

func newSessionCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "session",
		Short: "Print the active profile and its resolved values",
		RunE: func(cmd *cobra.Command, args []string) error {
			cfgDir := config.ConfigDir()
			profileFlag, _ := cmd.Flags().GetString("profile")
			p, err := config.LoadProfile(cfgDir, profileFlag)
			if err != nil {
				return err
			}
			out := cmd.OutOrStdout()
			fmt.Fprintf(out, "Profile:   %s\n", p.Name)
			fmt.Fprintf(out, "Server:    %s\n", p.Server)
			fmt.Fprintf(out, "Agent:     %s\n", p.Agent)
			return nil
		},
	}

	agentCmd := &cobra.Command{
		Use:   "agent [name]",
		Short: "Set the agent for the active profile",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			cfgDir := config.ConfigDir()
			profileFlag, _ := cmd.Flags().GetString("profile")
			return config.SetAgent(cfgDir, profileFlag, args[0])
		},
	}

	cmd.AddCommand(agentCmd)
	return cmd
}
