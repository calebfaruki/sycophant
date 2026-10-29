package cmd

import (
	"context"
	"errors"
	"fmt"
	"image/color"
	"strings"
	"time"

	"charm.land/bubbles/v2/key"
	"charm.land/bubbles/v2/textarea"
	"charm.land/bubbles/v2/viewport"
	tea "charm.land/bubbletea/v2"
	"charm.land/glamour/v2"
	glamouransi "charm.land/glamour/v2/ansi"
	"charm.land/glamour/v2/styles"
	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
	"google.golang.org/grpc"
	"google.golang.org/grpc/status"

	"github.com/sycophant-md/sycophant/cmd/syco/internal/config"
	"github.com/sycophant-md/sycophant/cmd/syco/internal/grpcclient"
	relaypb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/relay/v1"
	commonpb "github.com/sycophant-md/sycophant/cmd/syco/internal/proto/sycophant/common/v1"
)

// runTUI launches the interactive Bubble Tea TUI for multi-turn chat.
// When the profile names a workspace, a conversation named by --continue or
// --resume is opened first, so a refusal is returned before the program takes
// over the screen. Otherwise it is opened after the workspace picker, and a
// refusal quits the program.
func runTUI(
	cfgDir string,
	p *config.Profile,
	token string,
	continueFlag bool,
	resumeID string,
	grants []*commonpb.GrantSelection,
) error {
	m, err := openTUIModel(cfgDir, p, token, continueFlag, resumeID, grants)
	if err != nil {
		return err
	}
	final, err := tea.NewProgram(m).Run()
	if err != nil {
		return err
	}
	return final.(*tuiModel).openErr
}

// openTUIModel builds the TUI model for a profile and flags. When the
// profile names a workspace and the flags name a conversation, it also
// connects and opens that conversation: the stream is open before history
// and turn state are read, so no live frame after that point is lost.
func openTUIModel(
	cfgDir string,
	p *config.Profile,
	token string,
	continueFlag bool,
	resumeID string,
	grants []*commonpb.GrantSelection,
) (tea.Model, error) {
	m := newTUIModel(cfgDir, p, token, continueFlag, resumeID, grants)
	id := m.namedConversation()
	if p.Workspace == "" || id == "" {
		return m, nil
	}
	msg, err := connectChannelStream(token, p.Server, p.Workspace, nil)
	if err != nil {
		return nil, err
	}
	if err := msg.openConversation(token, p.Workspace, id); err != nil {
		msg.conn.Close()
		return nil, err
	}
	m.applyConnected(msg)
	return m, nil
}

// --- Phases ---

type tuiPhase int

const (
	phasePicker tuiPhase = iota // Workspace selection screen.
	phaseChat                   // Chat session.
)

// --- Styles ---

var (
	toolStyle      = lipgloss.NewStyle().Foreground(lipgloss.Color("6"))
	statusStyle    = lipgloss.NewStyle().Foreground(lipgloss.Color("8"))
	errorStyle     = lipgloss.NewStyle().Foreground(lipgloss.Color("1"))
	pickerTitle    = lipgloss.NewStyle().Foreground(lipgloss.Color("12")).Bold(true)
	pickerItem     = lipgloss.NewStyle().PaddingLeft(2)
	pickerSelected = lipgloss.NewStyle().PaddingLeft(2).Foreground(lipgloss.Color("10")).Bold(true)
	logoStyle      = lipgloss.NewStyle().Foreground(lipgloss.Color("#FFFFFF")).Bold(true)
)

var (
	panelBg           = lipgloss.Color("236")
	panelAccent       = lipgloss.Color("12")
	panelApproval     = lipgloss.Color("3")
	panelDisconnected = lipgloss.Color("1")
	connectedDot      = lipgloss.Color("10")
)

const logo = `█▀▀▀ █  █ █▀▀▀ █▀▀█ █▀▀█ █  █ █▀▀█ █▀▀▄ ▀██▀
▀▀▀█ ▀██▀ █    █  █ █▀▀▀ █▀▀█ █▀▀█ █  █  ██
▀▀▀▀  ▀▀  ▀▀▀▀ ▀▀▀▀ ▀    ▀  ▀ ▀  ▀ ▀  ▀  ▀▀`

func panelBar(c color.Color) string {
	return lipgloss.NewStyle().Foreground(c).Background(panelBg).Render("┃")
}

func panelRow(bar, r string, width int) string {
	right := max(width-1-panelPad-ansi.StringWidth(r), 0)
	return bar + panelFill.Render(strings.Repeat(" ", panelPad)) + r + panelFill.Render(strings.Repeat(" ", right))
}

var (
	panelFill       = lipgloss.NewStyle().Background(panelBg)
	panelDim        = statusStyle.Background(panelBg)
	panelTool       = toolStyle.Background(panelBg)
	panelAccentText = lipgloss.NewStyle().Foreground(panelAccent).Background(panelBg)
)

const panelPad = 2

// panelChrome counts the rows around the panel's body: its top padding,
// the blank row and info line under the body, its bottom edge, and the
// footer.
const panelChrome = 5

// --- Messages ---

type frameMsg struct {
	frame *commonpb.ChannelStreamOutbound
}

type streamErrMsg struct {
	err error
	// stream is the broken stream; nil for an ingest.
	stream grpc.ServerStreamingClient[commonpb.ChannelStreamOutbound]
}

// connectedMsg carries an open channel stream and, when conversationID is set, the
// conversation read through it.
type connectedMsg struct {
	conn            *grpc.ClientConn
	client          relaypb.RelayGatewayClient
	stream          grpc.ServerStreamingClient[commonpb.ChannelStreamOutbound]
	channelStreamID string

	conversationID string
	history        []*commonpb.HistoryEntry
	running        bool
}

type connErrMsg struct {
	err error
}

// openFailedMsg reports that a named conversation could not be opened.
// The program quits and the chat command returns the error.
type openFailedMsg struct {
	err error
}

type ingestAckMsg struct {
	conversationID string
}

type retryTickMsg struct{}

type answerErrMsg struct {
	err error
}

type cancelErrMsg struct {
	err error
}

// workspacesMsg carries the result of ListWorkspaces.
type workspacesMsg struct {
	workspaces []string
	conn       *grpc.ClientConn
	client     relaypb.RelayGatewayClient
}

// workspacesErrMsg signals a failure to list workspaces.
type workspacesErrMsg struct {
	err error
}

// --- Model ---

type tuiModel struct {
	phase tuiPhase

	// Config.
	cfgDir       string
	profile      *config.Profile
	token        string
	continueFlag bool
	resumeID     string
	grants       []*commonpb.GrantSelection

	// Connection state.
	conn            *grpc.ClientConn
	client          relaypb.RelayGatewayClient
	stream          grpc.ServerStreamingClient[commonpb.ChannelStreamOutbound]
	channelStreamID string
	connected       bool
	retryTick       time.Duration
	// backoff is the wait after the next failure; a reconnect resets it.
	retryIn int
	backoff time.Duration
	connErr string

	// Conversation state.
	conversationID string
	toolCalls      map[string]*toolRow
	textBuf        strings.Builder
	// running is the relay's latest turn state for the viewed conversation.
	running bool
	// sending covers an ingest in flight.
	sending  bool
	stopping bool
	queue    []string
	// approvals wait for a y or n key each, first in, first out. The first
	// is the one showing.
	approvals []toolApproval

	// Display. transcript is the finished conversation in order; its tool
	// rows are drawn with the view, so a running one can change in place.
	transcript []transcriptPart
	started    bool

	// Identity from token.
	subject string

	// Picker state.
	workspaces    []string
	pickerCursor  int
	pickerLoading bool

	// Chat UI components.
	viewport     viewport.Model
	input        textarea.Model
	ready        bool
	userAtBottom bool

	// Sent messages, oldest first, for Up/Down to recall. recallAt is the
	// one in the input while browsing, len(recall) when not; draft is
	// what the user had typed before browsing.
	recall       []string
	recallAt     int
	draft        string
	recallWarned bool

	// Terminal dimensions.
	width  int
	height int

	// Quitting.
	quitting bool
	err      error
	// openErr is a named conversation's open failure, returned by runTUI.
	openErr error
}

func newTUIModel(
	cfgDir string,
	p *config.Profile,
	token string,
	continueFlag bool,
	resumeID string,
	grants []*commonpb.GrantSelection,
) *tuiModel {
	ti := newChatInput()

	sub, _ := config.TokenSubject(token)
	email := config.TokenEmail(token)
	identity := email
	if identity == "" {
		identity = sub
	}

	// If the profile already has a workspace, skip the picker.
	phase := phasePicker
	if p.Workspace != "" {
		phase = phaseChat
		ti.Focus()
	}

	m := &tuiModel{
		phase:         phase,
		cfgDir:        cfgDir,
		profile:       p,
		token:         token,
		continueFlag:  continueFlag,
		resumeID:      resumeID,
		grants:        grants,
		toolCalls:     make(map[string]*toolRow),
		input:         ti,
		subject:       identity,
		pickerLoading: phase == phasePicker,
		userAtBottom:  true,
		retryTick:     time.Second,
		backoff:       initialBackoff,
	}
	m.recall = config.LoadInputHistory(cfgDir)
	m.recallAt = len(m.recall)
	return m
}

const inputMaxLines = 6

func newChatInput() textarea.Model {
	ta := textarea.New()
	ta.Placeholder = "Ask anything"
	ta.Prompt = ""
	ta.ShowLineNumbers = false
	ta.DynamicHeight = true
	ta.MaxHeight = inputMaxLines
	// Without a content cap the textarea stops new lines at MaxHeight; let
	// it take as many as its buffer holds and scroll instead.
	ta.MaxContentHeight = 10000
	// Some terminals send Shift+Enter as Escape then Enter, which arrives
	// as Alt+Enter.
	ta.KeyMap.InsertNewline = key.NewBinding(key.WithKeys("shift+enter", "ctrl+j", "alt+enter"))
	styles := ta.Styles()
	styles.Focused.Base = styles.Focused.Base.Background(panelBg)
	styles.Focused.CursorLine = lipgloss.NewStyle().Background(panelBg)
	styles.Focused.Prompt = lipgloss.NewStyle().Background(panelBg)
	styles.Focused.Placeholder = panelDim
	ta.SetStyles(styles)
	return ta
}

// namedConversation is the conversation the flags name for the profile's
// workspace, or "" for a new conversation.
func (m *tuiModel) namedConversation() string {
	if m.continueFlag {
		entry, err := config.HistoryLastForWorkspace(m.cfgDir, m.profile.Workspace)
		if err == nil {
			return entry.ConversationID
		}
		return ""
	}
	return m.resumeID
}

func (m *tuiModel) Init() tea.Cmd {
	if m.phase == phaseChat {
		if m.connected {
			return tea.Batch(textarea.Blink, m.recvCmd())
		}
		return tea.Batch(textarea.Blink, m.connectCmd())
	}
	// Picker phase: list workspaces first.
	return m.listWorkspacesCmd()
}

// --- Commands ---

// listWorkspacesCmd connects to the relay and fetches the workspace list.
func (m *tuiModel) listWorkspacesCmd() tea.Cmd {
	return func() tea.Msg {
		ctx := grpcclient.WithToken(context.Background(), m.token)
		conn, err := grpcclient.Dial(ctx, m.profile.Server)
		if err != nil {
			return workspacesErrMsg{err: fmt.Errorf("connecting to relay: %w", err)}
		}
		client := relaypb.NewRelayGatewayClient(conn)
		resp, err := client.ListWorkspaces(ctx, &commonpb.ListWorkspacesRequest{})
		if err != nil {
			conn.Close()
			return workspacesErrMsg{err: fmt.Errorf("listing workspaces: %w", err)}
		}
		return workspacesMsg{
			workspaces: resp.Workspaces,
			conn:       conn,
			client:     client,
		}
	}
}

// connectChannelStream dials the relay (or reuses conn), opens ChannelStreamReceive for
// the workspace and reads the ack.
func connectChannelStream(token, server, workspace string, conn *grpc.ClientConn) (connectedMsg, error) {
	ctx := grpcclient.WithToken(context.Background(), token)
	if conn == nil {
		var err error
		conn, err = grpcclient.Dial(ctx, server)
		if err != nil {
			return connectedMsg{}, fmt.Errorf("connecting to relay: %w", err)
		}
	}
	client := relaypb.NewRelayGatewayClient(conn)
	stream, err := client.ChannelStreamReceive(ctx, &commonpb.ChannelStreamReceiveRequest{Workspace: workspace})
	if err != nil {
		conn.Close()
		return connectedMsg{}, fmt.Errorf("channel stream receive: %w", err)
	}
	channelStreamID, err := readAck(stream)
	if err != nil {
		conn.Close()
		return connectedMsg{}, fmt.Errorf("reading channel stream ack: %w", err)
	}
	return connectedMsg{conn: conn, client: client, stream: stream, channelStreamID: channelStreamID}, nil
}

// openConversation reads a conversation's whole history and then its turn
// state through the open channel stream.
func (c *connectedMsg) openConversation(token, workspace, conversationID string) error {
	ctx := grpcclient.WithToken(context.Background(), token)
	history, err := loadHistory(ctx, c.client, conversationID, workspace)
	if err != nil {
		return err
	}
	ts, err := getTurnState(ctx, c.client, conversationID, workspace)
	if err != nil {
		return err
	}
	c.conversationID = conversationID
	c.history = history
	c.running = ts.GetState() == commonpb.TurnState_TURN_STATE_WORKING
	return nil
}

func (m *tuiModel) connectCmd() tea.Cmd {
	token, server, workspace, id := m.token, m.profile.Server, m.profile.Workspace, m.conversationID
	first := id == ""
	if first {
		id = m.namedConversation()
	}
	// Reuse existing connection from picker if available.
	conn, old, prefix := m.conn, (*grpc.ClientConn)(nil), ""
	if m.backoff > initialBackoff {
		conn, old, prefix = nil, m.conn, "reconnecting: "
	}
	return func() tea.Msg {
		if old != nil {
			old.Close()
		}
		msg, err := connectChannelStream(token, server, workspace, conn)
		if err != nil {
			return connErrMsg{err: fmt.Errorf("%s%w", prefix, err)}
		}
		if id != "" {
			if err := msg.openConversation(token, workspace, id); err != nil {
				msg.conn.Close()
				if first {
					return openFailedMsg{err: err}
				}
				return connErrMsg{err: fmt.Errorf("%s%w", prefix, err)}
			}
		}
		return msg
	}
}

func (m *tuiModel) recvCmd() tea.Cmd {
	stream := m.stream
	return func() tea.Msg {
		frame, err := stream.Recv()
		if err != nil {
			return streamErrMsg{err: err, stream: stream}
		}
		return frameMsg{frame: frame}
	}
}

func (m *tuiModel) ingestCmd(message string) tea.Cmd {
	client, channelStreamID, conversationID, grants, token := m.client, m.channelStreamID, m.conversationID, m.grants, m.token
	return func() tea.Msg {
		ctx := grpcclient.WithToken(context.Background(), token)
		resp, err := client.ChannelStreamIngest(ctx, userMessageIngest(channelStreamID, conversationID, message, grants))
		if err != nil {
			return streamErrMsg{err: fmt.Errorf("channel stream ingest: %w", err)}
		}
		return ingestAckMsg{conversationID: resp.ConversationId}
	}
}

func userMessageIngest(channelStreamID, conversationID, text string, grants []*commonpb.GrantSelection) *commonpb.ChannelStreamIngestRequest {
	return &commonpb.ChannelStreamIngestRequest{
		ChannelStreamId:  channelStreamID,
		ConversationId:   conversationID,
		SupportedMethods: supportedMethods,
		UserMessage: &commonpb.UserMessage{
			Content: []*commonpb.ContentBlock{{Block: &commonpb.ContentBlock_Text{Text: &commonpb.TextBlock{Text: text}}}},
			Grants:  grants,
		},
	}
}

func (m *tuiModel) cancelTurnCmd() tea.Cmd {
	client, conversationID, workspace, token := m.client, m.conversationID, m.profile.Workspace, m.token
	return func() tea.Msg {
		ctx := grpcclient.WithToken(context.Background(), token)
		if _, err := client.CancelTurn(ctx, &commonpb.CancelTurnRequest{
			ConversationId: conversationID,
			Workspace:      workspace,
		}); err != nil {
			return cancelErrMsg{err: fmt.Errorf("cancelling turn: %w", err)}
		}
		return nil
	}
}

func (m *tuiModel) answerApprovalCmd(requestID string, approved bool) tea.Cmd {
	client, channelStreamID, token := m.client, m.channelStreamID, m.token
	return func() tea.Msg {
		ctx := grpcclient.WithToken(context.Background(), token)
		if _, err := client.ChannelStreamIngest(ctx, approvalAnswer(channelStreamID, requestID, approved)); err != nil {
			return answerErrMsg{err: fmt.Errorf("sending approval answer: %w", err)}
		}
		return nil
	}
}

func (m *tuiModel) retryCmd() tea.Cmd {
	m.retryIn = int(m.backoff / time.Second)
	m.backoff = min(2*m.backoff, maxBackoff)
	return m.retryTickCmd()
}

func (m *tuiModel) retryTickCmd() tea.Cmd {
	return tea.Tick(m.retryTick, func(time.Time) tea.Msg {
		return retryTickMsg{}
	})
}

func (m *tuiModel) retryNotice() string {
	if m.retryIn > 0 {
		return fmt.Sprintf("reconnecting in %ds", m.retryIn)
	}
	return "reconnecting…"
}

// connReason is the reason err shows on screen: the relay's message when
// err carries a gRPC status, so the reason survives the cut to width.
func connReason(err error) string {
	var se interface{ GRPCStatus() *status.Status }
	if errors.As(err, &se) && se.GRPCStatus().Message() != "" {
		return se.GRPCStatus().Message()
	}
	return err.Error()
}

// applyConnected adopts an open channel stream. An opened conversation replaces
// the transcript with its history and sets the running state.
func (m *tuiModel) applyConnected(msg connectedMsg) {
	m.conn = msg.conn
	m.client = msg.client
	m.stream = msg.stream
	m.channelStreamID = msg.channelStreamID
	m.connected = true
	m.backoff = initialBackoff
	m.connErr = ""
	if msg.conversationID != "" {
		m.conversationID = msg.conversationID
		m.transcript = nil
		m.toolCalls = make(map[string]*toolRow)
		m.textBuf.Reset()
		m.renderHistory(msg.history)
		m.running = msg.running
		m.stopping = false
	}
}

// --- Update ---

func (m *tuiModel) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	if m.phase == phasePicker {
		return m.updatePicker(msg)
	}
	return m.updateChat(msg)
}

// --- Picker Update ---

func (m *tuiModel) updatePicker(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.KeyPressMsg:
		switch msg.String() {
		case "esc", "ctrl+c":
			m.quitting = true
			return m, tea.Quit
		case "up":
			if m.pickerCursor > 0 {
				m.pickerCursor--
			}
		case "down":
			if m.pickerCursor < len(m.workspaces)-1 {
				m.pickerCursor++
			}
		case "enter":
			if len(m.workspaces) > 0 {
				return m.selectWorkspace(m.workspaces[m.pickerCursor])
			}
		}

	case tea.WindowSizeMsg:
		m.width = msg.Width
		m.height = msg.Height

	case workspacesMsg:
		m.workspaces = msg.workspaces
		m.conn = msg.conn
		m.client = msg.client
		m.pickerLoading = false
		// If only one workspace, select it automatically.
		if len(m.workspaces) == 1 {
			return m.selectWorkspace(m.workspaces[0])
		}

	case workspacesErrMsg:
		m.pickerLoading = false
		m.err = msg.err
	}

	return m, nil
}

// selectWorkspace transitions from picker to chat.
func (m *tuiModel) selectWorkspace(workspace string) (tea.Model, tea.Cmd) {
	m.profile.Workspace = workspace
	m.phase = phaseChat
	m.input.SetWidth(inputWidth(m.panelWidth()))
	m.input.Focus()
	return m, tea.Batch(textarea.Blink, m.connectCmd())
}

// --- Chat Update ---

func (m *tuiModel) acceptsInput() bool {
	return len(m.approvals) == 0 && m.connected
}

func (m *tuiModel) sendMessage(text string) tea.Cmd {
	m.sending = true
	m.commitUser(text)
	m.remember(text)
	m.textBuf.Reset()
	m.syncViewport()
	return m.ingestCmd(text)
}

// remember adds a sent message to the recall history and saves it. A
// history that cannot be saved still recalls this session's messages.
// A queued message can be sent while the user browses; browsing then
// stays on the message it was on.
func (m *tuiModel) remember(text string) {
	fromEnd := len(m.recall) - m.recallAt
	if fromEnd > 0 && m.recall[len(m.recall)-1] != text {
		fromEnd++
	}
	saved, err := config.AppendInputHistory(m.cfgDir, text)
	if err != nil {
		m.recall = config.AddInputHistory(m.recall, text)
		if !m.recallWarned {
			m.recallWarned = true
			m.commitNotice(errorStyle.Render("⚠ saving message history: " + err.Error()))
		}
	} else {
		m.recall = saved
	}
	m.recallAt = max(len(m.recall)-fromEnd, 0)
}

func inputWidth(width int) int {
	return max(width-1-2*panelPad, 1)
}

const startPanelMax = 75

func (m *tuiModel) panelWidth() int {
	if m.started {
		return m.width
	}
	return max(min(startPanelMax, m.width-2*panelPad), 1)
}

func (m *tuiModel) approvalRows() []string {
	a := m.approvals[0]
	w := inputWidth(m.panelWidth())
	ask := fmt.Sprintf("⚙ %s wants to run with input: %s", a.tool, a.input)
	rows := strings.Split(ansi.Wrap(ask, w, ""), "\n")
	if n := inputMaxLines - 1; len(rows) > n {
		rows = rows[:n]
		rows[n-1] = ansi.Truncate(rows[n-1], w-1, "") + "…"
	}
	return append(rows, "y approve · n deny")
}

func (m *tuiModel) transcriptHeight() int {
	body := m.input.Height()
	if len(m.approvals) > 0 {
		body = len(m.approvalRows())
	}
	return max(m.height-panelChrome-body-len(m.queue), 1)
}

func (m *tuiModel) layout() {
	m.input.SetWidth(inputWidth(m.panelWidth()))
	m.viewport.SetWidth(m.width)
	m.viewport.SetHeight(m.transcriptHeight())
	m.syncViewport()
}

func (m *tuiModel) fitsLayout() bool {
	return !m.ready || (m.viewport.Height() == m.transcriptHeight() &&
		m.input.Width() == inputWidth(m.panelWidth()))
}

func scrollsTranscript(msg tea.Msg) bool {
	switch msg := msg.(type) {
	case tea.MouseMsg:
		return true
	case tea.KeyPressMsg:
		s := msg.String()
		return s == "pgup" || s == "pgdown"
	}
	return false
}

func (m *tuiModel) updateChat(msg tea.Msg) (tea.Model, tea.Cmd) {
	var cmds []tea.Cmd

	switch msg := msg.(type) {
	case tea.KeyPressMsg:
		if len(m.approvals) > 0 && (msg.Text == "y" || msg.Text == "n") {
			a := m.approvals[0]
			m.approvals = m.approvals[1:]
			return m, m.answerApprovalCmd(a.requestID, msg.Text == "y")
		}
		switch msg.String() {
		case "ctrl+c":
			m.quitting = true
			if m.conn != nil {
				m.conn.Close()
			}
			return m, tea.Quit

		case "esc":
			if len(m.queue) > 0 {
				m.returnQueueToInput()
				m.layout()
				return m, nil
			}
			if m.running && !m.stopping && m.conversationID != "" {
				m.stopping = true
				return m, m.cancelTurnCmd()
			}
			return m, nil

		case "up":
			if !m.acceptsInput() || m.recallAt == 0 || m.input.Line() > 0 || m.input.LineInfo().RowOffset > 0 {
				break
			}
			if m.recallAt == len(m.recall) {
				m.draft = m.input.Value()
			}
			m.recallAt--
			m.input.SetValue(m.recall[m.recallAt])
			m.input.MoveToBegin()
			m.input.CursorEnd()
			return m, nil

		case "down":
			info := m.input.LineInfo()
			if !m.acceptsInput() || m.recallAt == len(m.recall) || m.input.Line() != m.input.LineCount()-1 || info.RowOffset < info.Height-1 {
				break
			}
			m.recallAt++
			if m.recallAt == len(m.recall) {
				m.input.SetValue(m.draft)
				m.draft = ""
			} else {
				m.input.SetValue(m.recall[m.recallAt])
			}
			m.input.MoveToEnd()
			return m, nil

		case "enter":
			text := strings.TrimSpace(m.input.Value())
			if !m.acceptsInput() || text == "" {
				break
			}
			m.input.SetValue("")
			if m.running || m.sending {
				m.queue = append(m.queue, text)
				break
			}
			cmds = append(cmds, m.sendMessage(text))
		}

	case tea.WindowSizeMsg:
		m.width = msg.Width
		m.height = msg.Height
		if !m.ready {
			m.viewport = viewport.New(viewport.WithWidth(msg.Width), viewport.WithHeight(msg.Height))
			m.ready = true
		}
		m.layout()

	case connectedMsg:
		m.applyConnected(msg)
		m.syncViewport()
		cmds = append(cmds, m.recvCmd())

	case connErrMsg:
		m.connErr = oneLine(connReason(msg.err))
		cmds = append(cmds, m.retryCmd())

	case retryTickMsg:
		if m.quitting {
			break
		}
		m.retryIn--
		if m.retryIn > 0 {
			cmds = append(cmds, m.retryTickCmd())
		} else {
			cmds = append(cmds, m.connectCmd())
		}

	case answerErrMsg:
		m.commitNotice(errorStyle.Render("⚠ " + msg.err.Error()))
		m.syncViewport()

	case openFailedMsg:
		m.openErr = msg.err
		m.quitting = true
		return m, tea.Quit

	case ingestAckMsg:
		m.sending = false
		m.conversationID = msg.conversationID
		if msg.conversationID != "" {
			// The relay records WORKING for the conversation before it acks.
			m.running = true
			if err := config.AppendHistory(m.cfgDir, msg.conversationID, m.profile.Workspace); err != nil {
				m.commitNotice(errorStyle.Render("⚠ saving conversation history: " + err.Error()))
				m.syncViewport()
			}
		}

	case frameMsg:
		m.userAtBottom = m.viewport.AtBottom()
		cmds = append(cmds, m.handleFrameMsg(msg.frame))
		m.layout()
		cmds = append(cmds, m.recvCmd())

	case cancelErrMsg:
		m.stopping = false
		m.commitNotice(errorStyle.Render("⚠ " + msg.err.Error()))
		m.syncViewport()

	case streamErrMsg:
		if msg.stream == nil {
			m.sending = false
		}
		// A loss already being retried, or a stream an earlier reconnect
		// replaced: nothing more to do.
		if m.backoff > initialBackoff || (msg.stream != nil && msg.stream != m.stream) {
			break
		}
		// The stream is gone, or about to be: the reconnect closes its
		// connection. Show it until the reconnect lands.
		m.connected = false
		cmds = append(cmds, m.retryCmd())
	}

	if scrollsTranscript(msg) {
		var cmd tea.Cmd
		m.viewport, cmd = m.viewport.Update(msg)
		cmds = append(cmds, cmd)
	} else if m.acceptsInput() {
		var cmd tea.Cmd
		m.input, cmd = m.input.Update(msg)
		cmds = append(cmds, cmd)
	}

	if !m.fitsLayout() {
		m.layout()
	}

	return m, tea.Batch(cmds...)
}

// --- Frame handling ---

func (m *tuiModel) handleFrameMsg(frame *commonpb.ChannelStreamOutbound) tea.Cmd {
	switch c := frame.Command.(type) {
	case *commonpb.ChannelStreamOutbound_TurnState:
		ts := c.TurnState
		if ts.ConversationId == "" || ts.ConversationId != m.conversationID {
			return nil
		}
		m.running = ts.State == commonpb.TurnState_TURN_STATE_WORKING
		if !m.running {
			m.stopping = false
		}
		switch ts.State {
		case commonpb.TurnState_TURN_STATE_IDLE:
			m.flushStreamingToCommitted()
			m.endToolCalls(toolDone)
			m.input.Focus()
			if len(m.queue) > 0 && !m.sending {
				next := m.queue[0]
				m.queue = m.queue[1:]
				return m.sendMessage(next)
			}
		case commonpb.TurnState_TURN_STATE_FAILED, commonpb.TurnState_TURN_STATE_CANCELLED:
			notice := "⚠ Turn cancelled"
			if ts.State == commonpb.TurnState_TURN_STATE_FAILED {
				notice = "⚠ Turn failed: " + oneLine(ts.Reason)
			}
			m.flushStreamingToCommitted()
			m.endToolCalls(toolEnded)
			m.commitNotice(errorStyle.Render(notice))
			m.returnQueueToInput()
			m.input.Focus()
		}
	case *commonpb.ChannelStreamOutbound_StreamItem:
		item := c.StreamItem
		if item.ConversationId != m.conversationID {
			return nil
		}
		m.handleStreamItem(item)
	case *commonpb.ChannelStreamOutbound_ServerRequest:
		if a, ok := toolApprovalOf(c.ServerRequest); ok {
			m.approvals = append(m.approvals, a)
		}
	}
	return nil
}

func (m *tuiModel) returnQueueToInput() {
	if len(m.queue) == 0 {
		return
	}
	parts := m.queue
	if typed := m.input.Value(); typed != "" {
		parts = append([]string{typed}, parts...)
	}
	m.queue = nil
	m.input.SetValue(strings.Join(parts, " "))
	m.input.MoveToEnd()
	m.recallAt, m.draft = len(m.recall), ""
}

func (m *tuiModel) handleStreamItem(item *commonpb.StreamItem) {
	switch phase := item.Phase.(type) {
	case *commonpb.StreamItem_Start:
		if tu := phase.Start.GetToolUse(); tu != nil {
			m.flushStreamingToCommitted()
			row := &toolRow{name: tu.Name}
			m.toolCalls[item.ItemId] = row
			m.transcript = append(m.transcript, transcriptPart{kind: partTool, tool: row})
		}
	case *commonpb.StreamItem_Delta:
		if delta := phase.Delta.GetTextDelta(); delta != "" {
			m.textBuf.WriteString(delta)
		}
		if row, ok := m.toolCalls[item.ItemId]; ok {
			row.input.WriteString(phase.Delta.GetToolInputJson())
		}
	case *commonpb.StreamItem_Stop:
		if row, ok := m.toolCalls[item.ItemId]; ok {
			row.state = toolDone
			delete(m.toolCalls, item.ItemId)
		}
	}
}

func (m *tuiModel) endToolCalls(state toolState) {
	for id, row := range m.toolCalls {
		row.state = state
		delete(m.toolCalls, id)
	}
}

// --- Display helpers ---

const replyIndent = 1 + panelPad

type partKind int

const (
	partUser partKind = iota
	partReply
	partNotice
	partTool
)

type transcriptPart struct {
	kind       partKind
	text       string
	tool       *toolRow
	drawn      string
	drawnWidth int
}

func (p *transcriptPart) render(width int) string {
	indent := strings.Repeat(" ", replyIndent)
	switch p.kind {
	case partUser:
		return messagePanel(p.text, width)
	case partReply:
		if p.drawnWidth != width || p.drawn == "" {
			p.drawn, p.drawnWidth = renderReply(p.text, width), width
		}
		return p.drawn
	case partTool:
		return indent + p.tool.render(max(width-replyIndent, 1))
	default:
		return indent + p.text
	}
}

func messagePanel(text string, width int) string {
	w := inputWidth(width)
	rows := append([]string{""}, strings.Split(ansi.Wrap(text, w, ""), "\n")...)
	rows = append(rows, "")
	bar := panelBar(panelAccent)
	out := make([]string, len(rows))
	for i, r := range rows {
		out[i] = panelRow(bar, panelFill.Render(r), width)
	}
	return strings.Join(out, "\n")
}

// replyStyle is glamour's dark style with the document's margin set to
// replyIndent, so rendered text starts in that column, and without the
// blank line the document opens and closes with.
var replyStyle = func() glamouransi.StyleConfig {
	s := styles.DarkStyleConfig
	margin := uint(replyIndent)
	s.Document.Margin = &margin
	s.Document.BlockPrefix = ""
	s.Document.BlockSuffix = ""
	return s
}()

func renderReply(raw string, width int) string {
	r, _ := glamour.NewTermRenderer(glamour.WithStyles(replyStyle), glamour.WithWordWrap(width))
	if out, err := r.Render(raw); err == nil {
		lines := strings.Split(out, "\n")
		for len(lines) > 0 && isBlank(lines[0]) {
			lines = lines[1:]
		}
		for len(lines) > 0 && isBlank(lines[len(lines)-1]) {
			lines = lines[:len(lines)-1]
		}
		return strings.Join(lines, "\n")
	}
	return liveReply(raw, width)
}

func liveReply(raw string, width int) string {
	indent := strings.Repeat(" ", replyIndent)
	wrapped := ansi.Wrap(strings.TrimSpace(raw), max(width-2*replyIndent, 1), "")
	return indent + strings.ReplaceAll(wrapped, "\n", "\n"+indent)
}

func isBlank(s string) bool { return strings.TrimSpace(ansi.Strip(s)) == "" }

func (m *tuiModel) commitNotice(s string) {
	m.transcript = append(m.transcript, transcriptPart{kind: partNotice, text: s})
}

func (m *tuiModel) commitUser(text string) {
	m.started = true
	m.transcript = append(m.transcript, transcriptPart{kind: partUser, text: text})
}

// markdown.
func (m *tuiModel) commitAssistant(raw string) {
	m.started = true
	m.transcript = append(m.transcript, transcriptPart{kind: partReply, text: raw})
}

func (m *tuiModel) flushStreamingToCommitted() {
	if m.textBuf.Len() == 0 {
		return
	}
	m.commitAssistant(m.textBuf.String())
	m.textBuf.Reset()
}

// renderHistory commits a conversation's history: user and assistant text
// and the assistant's tool calls. Tool results, tagged sub-agent entries
// and non-text blocks are not shown.
func (m *tuiModel) renderHistory(entries []*commonpb.HistoryEntry) {
	for _, e := range entries {
		msg := e.GetMessage()
		if e.Tag != nil || msg == nil {
			continue
		}
		switch msg.GetRole() {
		case "user":
			m.commitUser(entryText(msg))
		case "assistant":
			if text := entryText(msg); text != "" {
				m.commitAssistant(text)
			}
			for _, tc := range msg.GetToolCalls() {
				row := &toolRow{name: tc.GetName(), state: toolDone}
				row.input.WriteString(tc.GetInputJson())
				m.transcript = append(m.transcript, transcriptPart{kind: partTool, tool: row})
			}
		}
	}
}

func (m *tuiModel) syncViewport() {
	if !m.ready {
		return
	}
	var full strings.Builder
	prevTool := false
	block := func(s string, tool bool) {
		if isBlank(s) {
			return // An empty block would leave two blank lines.
		}
		switch {
		case full.Len() == 0:
		case tool && prevTool:
			full.WriteString("\n") // A turn's tool rows sit together.
		default:
			full.WriteString("\n\n")
		}
		full.WriteString(s)
		prevTool = tool
	}
	for i := range m.transcript {
		p := &m.transcript[i]
		block(p.render(m.width), p.kind == partTool)
	}
	if m.textBuf.Len() > 0 {
		block(liveReply(m.textBuf.String(), m.width), false)
	}
	content := full.String()
	if content != "" {
		content += "\n"
	}
	m.viewport.SetContent(lipgloss.PlaceVertical(m.viewport.Height(), lipgloss.Bottom, content))
	if m.userAtBottom {
		m.viewport.GotoBottom()
	}
}

// --- View ---

func (m *tuiModel) View() tea.View {
	var v tea.View
	v.AltScreen = true
	v.MouseMode = tea.MouseModeAllMotion
	if m.quitting {
		return v
	}

	switch m.phase {
	case phasePicker:
		v.SetContent(m.viewPicker())
	case phaseChat:
		v.SetContent(m.viewChat())
	}
	return v
}

func (m *tuiModel) viewPicker() string {
	var b strings.Builder

	b.WriteString("\n")
	b.WriteString(pickerTitle.Render("  sycophant"))
	b.WriteString("\n")
	b.WriteString(statusStyle.Render(fmt.Sprintf("  %s · %s", m.subject, m.profile.Server)))
	b.WriteString("\n\n")

	if m.pickerLoading {
		b.WriteString(statusStyle.Render("  Loading workspaces…"))
		b.WriteString("\n")
		return b.String()
	}

	if m.err != nil {
		b.WriteString(errorStyle.Render("  ⚠ " + m.err.Error()))
		b.WriteString("\n")
		return b.String()
	}

	if len(m.workspaces) == 0 {
		b.WriteString(statusStyle.Render("  No workspaces available."))
		b.WriteString("\n")
		return b.String()
	}

	b.WriteString(statusStyle.Render("  Select a workspace:"))
	b.WriteString("\n\n")

	for i, ws := range m.workspaces {
		if i == m.pickerCursor {
			b.WriteString(pickerSelected.Render("› " + ws))
		} else {
			b.WriteString(pickerItem.Render("  " + ws))
		}
		b.WriteString("\n")
	}

	b.WriteString("\n")
	b.WriteString(statusStyle.Render("  ↑/↓ navigate · enter select · esc quit"))
	b.WriteString("\n")

	return b.String()
}

func (m *tuiModel) viewChat() string {
	if !m.ready {
		return "Connecting…"
	}
	if !m.started {
		return m.viewStart()
	}
	return m.viewport.View() + "\n" + m.viewQueued(m.width) + m.viewPanel(m.width) + "\n" + m.viewFooter(m.width)
}

func (m *tuiModel) viewQueued(width int) string {
	var queued strings.Builder
	for _, q := range m.queue {
		q = strings.ReplaceAll(q, "\n", " ⏎ ")
		queued.WriteString(statusStyle.Render(ansi.Truncate("queued: "+q, width, "…")))
		queued.WriteString("\n")
	}
	return queued.String()
}

func (m *tuiModel) viewStart() string {
	pw := m.panelWidth()
	margin := strings.Repeat(" ", max((m.width-pw)/2, 0))
	block := m.viewQueued(pw) + m.viewPanel(pw) + "\n" + m.viewFooter(pw)
	if m.backoff > initialBackoff {
		notice := "⚠ Stream disconnected"
		if m.connErr != "" {
			notice = "⚠ " + m.connErr
		}
		progress := m.retryNotice()
		if m.retryIn > 0 {
			progress += "…"
		}
		block += "\n" + lineWithHints(pw, errorStyle.Render(notice)+statusStyle.Render(" · "+progress), "")
	}
	group := margin + strings.ReplaceAll(block, "\n", "\n"+margin)

	room := m.height - 1
	logoW, logoH := lipgloss.Size(logo)
	if m.width >= logoW && room >= logoH+1+lipgloss.Height(group) {
		group = lipgloss.PlaceHorizontal(m.width, lipgloss.Center, logoStyle.Render(logo)) + "\n\n" + group
	}

	dot := lipgloss.NewStyle().Foreground(connectedDot)
	if !m.connected {
		dot = dot.Foreground(panelDisconnected)
	}
	corners := lineWithHints(m.width, dot.Render("●")+" "+statusStyle.Render(m.profile.Server), statusStyle.Render(version))
	lines := strings.Split(lipgloss.PlaceVertical(room, lipgloss.Center, group)+"\n"+corners, "\n")
	return strings.Join(lines[max(len(lines)-m.height, 0):], "\n")
}

func lineWithHints(width int, left, right string) string {
	pad := strings.Repeat(" ", panelPad)
	room := max(width-2*panelPad, 0)
	lw, rw := ansi.StringWidth(left), ansi.StringWidth(right)
	if right == "" || lw+2+rw > room {
		return pad + ansi.Truncate(left, room, "…")
	}
	return pad + left + strings.Repeat(" ", room-lw-rw) + right
}

func (m *tuiModel) viewPanel(width int) string {
	var body []string
	if len(m.approvals) > 0 {
		for _, r := range m.approvalRows() {
			body = append(body, panelTool.Render(r))
		}
	} else {
		body = strings.Split(m.input.View(), "\n")
	}
	w := inputWidth(width)
	info := ansi.Truncate(panelAccentText.Render("Default")+panelDim.Render(" · "+m.profile.Workspace), w, "…")

	rows := append([]string{""}, body...)
	rows = append(rows, "", info)
	c := panelAccent
	switch {
	case !m.connected:
		c = panelDisconnected
	case len(m.approvals) > 0:
		c = panelApproval
	}
	bar := panelBar(c)
	var b strings.Builder
	for _, r := range rows {
		b.WriteString(panelRow(bar, ansi.Truncate(r, w, ""), width) + "\n")
	}
	b.WriteString(lipgloss.NewStyle().Foreground(c).Render("╹"))
	b.WriteString(lipgloss.NewStyle().Foreground(panelBg).Render(strings.Repeat("▀", max(width-1, 0))))
	return b.String()
}

func (m *tuiModel) viewFooter(width int) string {
	switch {
	case !m.connected:
		left := errorStyle.Render("disconnected")
		if m.started && m.backoff > initialBackoff {
			progress := " · " + m.retryNotice()
			room := width - 2*panelPad - ansi.StringWidth("disconnected · "+progress)
			if m.connErr != "" && room >= 2 {
				left = errorStyle.Render("disconnected · " + ansi.Truncate(m.connErr, room, "…"))
			}
			left += statusStyle.Render(progress)
		}
		return lineWithHints(width, left, "")
	case m.stopping:
		return lineWithHints(width, statusStyle.Render("stopping…"), "")
	case m.running:
		return lineWithHints(width, statusStyle.Render("working…"), statusStyle.Render("esc stop"))
	}
	identity := m.subject
	if identity == "" {
		identity = "unknown"
	}
	return lineWithHints(width, statusStyle.Render(identity+" · "+m.profile.Server), statusStyle.Render("shift+enter newline"))
}
