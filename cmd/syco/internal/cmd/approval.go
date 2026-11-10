package cmd

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"strings"

	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

const requestToolApprovalMethod = "RequestToolApproval"

// supportedMethods is sent on every ingest. The relay replaces a channel
// stream's advertised set from each ingest that carries one.
var supportedMethods = []string{requestToolApprovalMethod}

// toolApproval is a request to approve one tool call before it runs.
type toolApproval struct {
	requestID string
	tool      string
	input     string
}

func (a toolApproval) prompt() string {
	return fmt.Sprintf("⚙ %s wants to run with input: %s", a.tool, a.input)
}

// toolApprovalOf reads a ServerRequest as an approval request. A request
// with no id is a notification and gets no answer, so it is not one.
func toolApprovalOf(sr *commonpb.ServerRequest) (toolApproval, bool) {
	if sr.GetRequestId() == "" {
		return toolApproval{}, false
	}
	var params struct {
		Tool  string `json:"tool"`
		Input string `json:"input"`
	}
	_ = json.Unmarshal([]byte(sr.GetParamsJson()), &params)
	return toolApproval{requestID: sr.GetRequestId(), tool: params.Tool, input: params.Input}, true
}

// approvalAnswer is the ingest that answers an approval request.
func approvalAnswer(channelStreamID, requestID string, approved bool) *commonpb.ChannelStreamIngestRequest {
	result, _ := json.Marshal(struct {
		Approved bool `json:"approved"`
	}{approved})
	return &commonpb.ChannelStreamIngestRequest{
		ChannelStreamId:  channelStreamID,
		SupportedMethods: supportedMethods,
		ClientResponse: &commonpb.ClientResponse{
			RequestId:  requestID,
			ResultJson: string(result),
		},
	}
}

func approvesLine(line string) bool {
	switch strings.ToLower(strings.TrimSpace(line)) {
	case "y", "yes":
		return true
	}
	return false
}

// readLines hands r's lines over one at a time and closes the channel at
// end of input. It holds at most one line while it waits for it to be taken.
func readLines(r io.Reader) <-chan string {
	lines := make(chan string)
	go func() {
		defer close(lines)
		br := bufio.NewReader(r)
		for {
			line, err := br.ReadString('\n')
			if line != "" {
				lines <- line
			}
			if err != nil {
				return
			}
		}
	}()
	return lines
}
