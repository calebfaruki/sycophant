package cmd

import (
	"context"
	"fmt"

	"github.com/spf13/cobra"

	"github.com/sycophant-md/sycophant/cmd/syco/internal/config"
	"github.com/sycophant-md/sycophant/cmd/syco/internal/grpcclient"
	relaypb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/relay/v1"
	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

func newWhoamiCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "whoami",
		Short: "Print the current user's identity and reachable agents",
		RunE: func(cmd *cobra.Command, args []string) error {
			cfgDir := config.ConfigDir()
			profileFlag, _ := cmd.Flags().GetString("profile")
			p, err := config.LoadProfile(cfgDir, profileFlag)
			if err != nil {
				return err
			}

			token, err := config.LoadToken(cfgDir, p.Name)
			if err != nil {
				return err
			}

			sub, _ := config.TokenSubject(token)
			exp, _ := config.TokenExpiry(token)
			out := cmd.OutOrStdout()
			fmt.Fprintf(out, "Subject: %s\n", sub)
			fmt.Fprintf(out, "Expires: %s\n", exp.UTC().Format("2006-01-02 15:04:05 UTC"))

			ctx := grpcclient.WithToken(context.Background(), token)
			conn, err := grpcclient.Dial(p.Server)
			if err != nil {
				return fmt.Errorf("connecting to relay: %w", err)
			}
			defer conn.Close()

			client := relaypb.NewRelayGatewayClient(conn)
			agentsResp, err := client.ListAgents(ctx, &commonpb.ListAgentsRequest{})
			if err != nil {
				fmt.Fprintf(cmd.ErrOrStderr(), "warning: could not list agents: %v\n", err)
				return nil
			}

			fmt.Fprintln(out, "\nAgents:")
			for _, agent := range agentsResp.Agents {
				fmt.Fprintf(out, "  %s\n", agent)
			}
			return nil
		},
	}
}
