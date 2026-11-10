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

// runTUI launches the interactive Bubble Tea TUI for multi-turn chat on the
// agent's one conversation.
func runTUI(cfgDir string, p *config.Profile, token string) error {
	_, err := tea.NewProgram(newTUIModel(cfgDir, p, token)).Run()
	return err
}

const (
	initialBackoff = time.Second
	maxBackoff     = 30 * time.Second
)

// --- Phases ---

type tuiPhase int

const (
	phasePicker tuiPhase = iota
	phaseChat
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
	stream grpc.ServerStreamingClient[commonpb.ChannelStreamOutbound]
}

// ingestErrMsg is a user message the relay did not take.
type ingestErrMsg struct {
	text string
	err  error
}

// connectedMsg carries an open channel stream. connectCmd adds the agent's
// conversation, read through it.
type connectedMsg struct {
	conn            *grpc.ClientConn
	client          relaypb.RelayGatewayClient
	stream          grpc.ServerStreamingClient[commonpb.ChannelStreamOutbound]
	channelStreamID string

	history []*commonpb.HistoryEntry
	running bool
}

type connErrMsg struct {
	err error
}

// historyMsg carries the conversation read at a turn's start or end. gen
// names the reload it answers; a later reload supersedes it.
type historyMsg struct {
	gen     int
	entries []*commonpb.HistoryEntry
	err     error
}

// takeBackMsg is the answer to a take-back: the text taken, if any.
type takeBackMsg struct {
	taken bool
	text  string
	err   error
}

// contextMsg is the outcome of /clear or /compact: notice when it ran, err
// when it or the reload after it failed.
type contextMsg struct {
	notice  string
	entries []*commonpb.HistoryEntry
	err     error
}

type retryTickMsg struct{}

type answerErrMsg struct {
	err error
}

type cancelErrMsg struct {
	err error
}

type agentsMsg struct {
	agents []string
	conn   *grpc.ClientConn
	client relaypb.RelayGatewayClient
}

type agentsErrMsg struct {
	err error
}

// --- Model ---

type tuiModel struct {
	phase tuiPhase

	cfgDir  string
	profile *config.Profile
	token   string

	conn            *grpc.ClientConn
	client          relaypb.RelayGatewayClient
	stream          grpc.ServerStreamingClient[commonpb.ChannelStreamOutbound]
	channelStreamID string
	connected       bool
	retryTick       time.Duration
	retryIn         int
	// backoff is the wait after the next failure; a reconnect resets it.
	backoff time.Duration
	connErr string

	toolCalls map[string]*toolRow
	textBuf   strings.Builder
	// running is the relay's latest turn state for the agent.
	running    bool
	stopping   bool
	reloadGen  int
	reloadFrom int
	// approvals wait for a y or n key each, first in, first out. The first
	// is the one showing.
	approvals []toolApproval

	// transcript is the finished conversation in order; its tool rows are
	// drawn with the view, so a running one can change in place.
	transcript []transcriptPart
	started    bool

	// Identity from token.
	subject string

	agents        []string
	pickerCursor  int
	pickerLoading bool

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

	width  int
	height int

	quitting bool
	err      error
}

func newTUIModel(cfgDir string, p *config.Profile, token string) *tuiModel {
	ti := newChatInput()

	sub, _ := config.TokenSubject(token)
	email := config.TokenEmail(token)
	identity := email
	if identity == "" {
		identity = sub
	}

	phase := phasePicker
	if p.Agent != "" {
		phase = phaseChat
		ti.Focus()
	}

	m := &tuiModel{
		phase:         phase,
		cfgDir:        cfgDir,
		profile:       p,
		token:         token,
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

func (m *tuiModel) Init() tea.Cmd {
	if m.phase == phaseChat {
		return tea.Batch(textarea.Blink, m.connectCmd())
	}
	return m.listAgentsCmd()
}

// --- Commands ---

func (m *tuiModel) listAgentsCmd() tea.Cmd {
	return func() tea.Msg {
		ctx := grpcclient.WithToken(context.Background(), m.token)
		conn, err := grpcclient.Dial(m.profile.Server)
		if err != nil {
			return agentsErrMsg{err: fmt.Errorf("connecting to relay: %w", err)}
		}
		client := relaypb.NewRelayGatewayClient(conn)
		resp, err := client.ListAgents(ctx, &commonpb.ListAgentsRequest{})
		if err != nil {
			conn.Close()
			return agentsErrMsg{err: fmt.Errorf("listing agents: %s", connReason(err))}
		}
		return agentsMsg{
			agents: resp.Agents,
			conn:   conn,
			client: client,
		}
	}
}

func loadHistory(ctx context.Context, client relaypb.RelayGatewayClient, agent string) ([]*commonpb.HistoryEntry, error) {
	resp, err := client.GetConversationHistory(ctx, &commonpb.GetConversationHistoryRequest{Agent: agent})
	if err != nil {
		return nil, fmt.Errorf("loading the conversation of %s: %s", agent, connReason(err))
	}
	return resp.GetEntries(), nil
}

func (m *tuiModel) connectCmd() tea.Cmd {
	token, server, agent := m.token, m.profile.Server, m.profile.Agent
	// Reuse existing connection from picker if available.
	conn, old := m.conn, (*grpc.ClientConn)(nil)
	if m.backoff > initialBackoff {
		conn, old = nil, m.conn
	}
	return func() tea.Msg {
		ctx := grpcclient.WithToken(context.Background(), token)
		if old != nil {
			old.Close()
		}
		msg, err := connectChannelStream(ctx, server, agent, conn)
		if err != nil {
			return connErrMsg{err: err}
		}
		// Read after the stream is open, so no live frame is lost.
		history, err := loadHistory(ctx, msg.client, agent)
		if err != nil {
			msg.conn.Close()
			return connErrMsg{err: err}
		}
		ts, err := msg.client.GetTurnState(ctx, &commonpb.GetTurnStateRequest{Agent: agent})
		if err != nil {
			msg.conn.Close()
			return connErrMsg{err: err}
		}
		msg.history = history
		msg.running = ts.GetState() == commonpb.TurnState_TURN_STATE_WORKING
		return msg
	}
}

func (m *tuiModel) recvCmd() tea.Cmd {
	stream := m.stream
	return func() tea.Msg {
		frame, err := stream.Recv()
		if err != nil {
			return streamErrMsg{stream: stream}
		}
		return frameMsg{frame: frame}
	}
}

func (m *tuiModel) ingestCmd(message string) tea.Cmd {
	client, channelStreamID, token := m.client, m.channelStreamID, m.token
	return func() tea.Msg {
		ctx := grpcclient.WithToken(context.Background(), token)
		if _, err := client.ChannelStreamIngest(ctx, userMessageIngest(channelStreamID, message)); err != nil {
			return ingestErrMsg{text: message, err: err}
		}
		return nil
	}
}

func userMessageIngest(channelStreamID, text string) *commonpb.ChannelStreamIngestRequest {
	return &commonpb.ChannelStreamIngestRequest{
		ChannelStreamId:  channelStreamID,
		SupportedMethods: supportedMethods,
		UserMessage: &commonpb.UserMessage{
			Content: []*commonpb.ContentBlock{{Block: &commonpb.ContentBlock_Text{Text: &commonpb.TextBlock{Text: text}}}},
		},
	}
}

func (m *tuiModel) cancelTurnCmd() tea.Cmd {
	client, agent, token := m.client, m.profile.Agent, m.token
	return func() tea.Msg {
		ctx := grpcclient.WithToken(context.Background(), token)
		if _, err := client.CancelTurn(ctx, &commonpb.CancelTurnRequest{Agent: agent}); err != nil {
			return cancelErrMsg{err: fmt.Errorf("cancelling turn: %s", connReason(err))}
		}
		return nil
	}
}

// takeBackCmd asks for the caller's newest message still waiting in the
// agent's queue.
func (m *tuiModel) takeBackCmd() tea.Cmd {
	client, agent, token := m.client, m.profile.Agent, m.token
	return func() tea.Msg {
		ctx := grpcclient.WithToken(context.Background(), token)
		resp, err := client.TakeBackMessage(ctx, &commonpb.TakeBackMessageRequest{Agent: agent})
		if err != nil {
			return takeBackMsg{err: fmt.Errorf("taking back a message: %s", connReason(err))}
		}
		return takeBackMsg{taken: resp.GetTaken(), text: entryText(resp.GetContent())}
	}
}

func (m *tuiModel) historyCmd(gen int) tea.Cmd {
	client, agent, token := m.client, m.profile.Agent, m.token
	return func() tea.Msg {
		ctx := grpcclient.WithToken(context.Background(), token)
		entries, err := loadHistory(ctx, client, agent)
		return historyMsg{gen: gen, entries: entries, err: err}
	}
}

// contextCmd runs /clear or /compact and then reads the conversation it
// leaves.
func (m *tuiModel) contextCmd(command string) tea.Cmd {
	client, agent, token := m.client, m.profile.Agent, m.token
	return func() tea.Msg {
		ctx := grpcclient.WithToken(context.Background(), token)
		var err error
		notice := "context cleared"
		if command == "/compact" {
			notice = "context compacted"
			_, err = client.CompactContext(ctx, &commonpb.CompactContextRequest{Agent: agent})
		} else {
			_, err = client.ClearContext(ctx, &commonpb.ClearContextRequest{Agent: agent})
		}
		if err != nil {
			return contextMsg{err: fmt.Errorf("%s: %s", command, connReason(err))}
		}
		entries, err := loadHistory(ctx, client, agent)
		return contextMsg{notice: notice, entries: entries, err: err}
	}
}

func (m *tuiModel) answerApprovalCmd(requestID string, approved bool) tea.Cmd {
	client, channelStreamID, token := m.client, m.channelStreamID, m.token
	return func() tea.Msg {
		ctx := grpcclient.WithToken(context.Background(), token)
		if _, err := client.ChannelStreamIngest(ctx, approvalAnswer(channelStreamID, requestID, approved)); err != nil {
			return answerErrMsg{err: fmt.Errorf("sending approval answer: %s", connReason(err))}
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

// applyConnected adopts an open channel stream: the transcript becomes the
// conversation's history and the running state the agent's.
func (m *tuiModel) applyConnected(msg connectedMsg) {
	m.conn = msg.conn
	m.client = msg.client
	m.stream = msg.stream
	m.channelStreamID = msg.channelStreamID
	m.connected = true
	m.backoff = initialBackoff
	m.connErr = ""
	old := m.transcript
	m.transcript = nil
	m.toolCalls = make(map[string]*toolRow)
	m.textBuf.Reset()
	m.renderHistory(msg.history)
	m.transcript = keepNotices(old, m.transcript)
	m.running = msg.running
	m.stopping = false
	m.reloadGen++
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
			if m.pickerCursor < len(m.agents)-1 {
				m.pickerCursor++
			}
		case "enter":
			if len(m.agents) > 0 {
				return m.selectAgent(m.agents[m.pickerCursor])
			}
		}

	case tea.WindowSizeMsg:
		m.width = msg.Width
		m.height = msg.Height

	case agentsMsg:
		m.agents = msg.agents
		m.conn = msg.conn
		m.client = msg.client
		m.pickerLoading = false
		if len(m.agents) == 1 {
			return m.selectAgent(m.agents[0])
		}

	case agentsErrMsg:
		m.pickerLoading = false
		m.err = msg.err
	}

	return m, nil
}

func (m *tuiModel) selectAgent(agent string) (tea.Model, tea.Cmd) {
	m.profile.Agent = agent
	m.phase = phaseChat
	m.input.SetWidth(inputWidth(m.panelWidth()))
	m.input.Focus()
	return m, tea.Batch(textarea.Blink, m.connectCmd())
}

// --- Chat Update ---

func (m *tuiModel) acceptsInput() bool {
	return len(m.approvals) == 0 && m.connected
}

// sendMessage sends text at once and draws nothing of its own. It shows
// when the transcript reloads at its own turn's start.
func (m *tuiModel) sendMessage(text string) tea.Cmd {
	m.remember(text)
	m.started = true
	m.syncViewport()
	return m.ingestCmd(text)
}

// remember adds a sent message to the recall history and saves it. A
// history that cannot be saved still recalls this session's messages.
// A message can be sent while the user browses; browsing then stays on
// the message it was on.
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

// returnToInput puts text back in the input after whatever is typed there.
func (m *tuiModel) returnToInput(text string) {
	if typed := m.input.Value(); typed != "" {
		text = typed + " " + text
	}
	m.input.SetValue(text)
	m.input.MoveToEnd()
	m.recallAt, m.draft = len(m.recall), ""
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
	rows := strings.Split(ansi.Wrap(a.prompt(), w, ""), "\n")
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
	return max(m.height-panelChrome-body, 1)
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
			if !m.connected {
				return m, nil
			}
			return m, m.takeBackCmd()

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
			if text == "/clear" || text == "/compact" {
				cmds = append(cmds, m.contextCmd(text))
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

	case takeBackMsg:
		switch {
		case msg.err != nil:
			m.commitNotice(errorStyle.Render("⚠ " + msg.err.Error()))
			m.syncViewport()
		case msg.taken:
			m.returnToInput(msg.text)
			m.syncViewport()
		case m.running && !m.stopping:
			m.stopping = true
			cmds = append(cmds, m.cancelTurnCmd())
		}

	case ingestErrMsg:
		m.commitNotice(errorStyle.Render("⚠ sending message: " + connReason(msg.err)))
		m.returnToInput(msg.text)
		m.syncViewport()

	case historyMsg:
		if msg.gen != m.reloadGen {
			break
		}
		if msg.err != nil {
			m.commitNotice(errorStyle.Render("⚠ " + msg.err.Error()))
		} else {
			old := m.transcript[:min(m.reloadFrom, len(m.transcript))]
			live := m.transcript[len(old):]
			m.transcript = nil
			m.renderHistory(msg.entries)
			m.transcript = append(keepNotices(old, m.transcript), live...)
		}
		m.syncViewport()

	case contextMsg:
		// The start screen hides the transcript; leave it so the result
		// shows even on an empty conversation.
		m.started = true
		if msg.notice != "" {
			m.reloadGen++
			m.transcript = nil
			m.toolCalls = make(map[string]*toolRow)
			m.textBuf.Reset()
			m.renderHistory(msg.entries)
			m.commitNotice(statusStyle.Render(msg.notice))
		}
		if msg.err != nil {
			m.commitNotice(errorStyle.Render("⚠ " + msg.err.Error()))
		}
		m.syncViewport()

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
		// A stream an earlier reconnect replaced: nothing more to do.
		if msg.stream != m.stream {
			break
		}
		// The stream is gone. Show it until the reconnect lands.
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
		m.running = ts.State == commonpb.TurnState_TURN_STATE_WORKING
		if !m.running {
			m.stopping = false
		}
		switch ts.State {
		case commonpb.TurnState_TURN_STATE_WORKING:
			// A turn start names its message. Reload the transcript so it
			// shows the message the turn runs on, whoever sent it.
			if id := ts.GetMessageId(); id != "" {
				m.flushStreamingToCommitted()
				m.reloadGen++
				m.reloadFrom = len(m.transcript)
				return m.historyCmd(m.reloadGen)
			}
		case commonpb.TurnState_TURN_STATE_IDLE:
			// The log holds the reply now. Reload so it shows once, even if
			// the connect's history read already had it.
			m.flushStreamingToCommitted()
			m.endToolCalls(toolDone)
			m.input.Focus()
			m.reloadGen++
			m.reloadFrom = len(m.transcript)
			return m.historyCmd(m.reloadGen)
		case commonpb.TurnState_TURN_STATE_FAILED, commonpb.TurnState_TURN_STATE_CANCELLED:
			notice := "⚠ Turn cancelled"
			if ts.State == commonpb.TurnState_TURN_STATE_FAILED {
				notice = "⚠ Turn failed: " + oneLine(ts.Reason)
			}
			m.flushStreamingToCommitted()
			m.endToolCalls(toolEnded)
			m.commitNotice(errorStyle.Render(notice))
			m.input.Focus()
		}
	case *commonpb.ChannelStreamOutbound_StreamItem:
		m.handleStreamItem(c.StreamItem)
	case *commonpb.ChannelStreamOutbound_ServerRequest:
		if a, ok := toolApprovalOf(c.ServerRequest); ok {
			m.approvals = append(m.approvals, a)
		}
	}
	return nil
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

// keepNotices puts old's notices into rendered, a fresh render of the
// history. Notices aren't in history, so each goes back before the user
// message that followed it in old, or at the end if none did.
func keepNotices(old, rendered []transcriptPart) []transcriptPart {
	var out []transcriptPart
	for _, p := range rendered {
		if p.kind == partUser {
			for ; len(old) > 0 && old[0].kind != partUser; old = old[1:] {
				if old[0].kind == partNotice {
					out = append(out, old[0])
				}
			}
			if len(old) > 0 {
				old = old[1:]
			}
		}
		out = append(out, p)
	}
	for _, p := range old {
		if p.kind == partNotice {
			out = append(out, p)
		}
	}
	return out
}

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
// and the assistant's tool calls. Tool results and non-text blocks are not
// shown.
func (m *tuiModel) renderHistory(entries []*commonpb.HistoryEntry) {
	for _, e := range entries {
		msg := e.GetMessage()
		if msg == nil {
			continue
		}
		switch msg.GetRole() {
		case "user":
			m.started = true
			m.transcript = append(m.transcript, transcriptPart{kind: partUser, text: entryText(msg.GetContent())})
		case "assistant":
			if text := entryText(msg.GetContent()); text != "" {
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

func entryText(blocks []*commonpb.ContentBlock) string {
	var b strings.Builder
	for _, block := range blocks {
		if tb := block.GetText(); tb != nil {
			b.WriteString(tb.Text)
		}
	}
	return b.String()
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
		b.WriteString(statusStyle.Render("  Loading agents…"))
		b.WriteString("\n")
		return b.String()
	}

	if m.err != nil {
		b.WriteString(errorStyle.Render("  ⚠ " + m.err.Error()))
		b.WriteString("\n")
		return b.String()
	}

	if len(m.agents) == 0 {
		b.WriteString(statusStyle.Render("  No agents available."))
		b.WriteString("\n")
		return b.String()
	}

	b.WriteString(statusStyle.Render("  Select an agent:"))
	b.WriteString("\n\n")

	for i, agent := range m.agents {
		if i == m.pickerCursor {
			b.WriteString(pickerSelected.Render("› " + agent))
		} else {
			b.WriteString(pickerItem.Render("  " + agent))
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
	return m.viewport.View() + "\n" + m.viewPanel(m.width) + "\n" + m.viewFooter(m.width)
}

func (m *tuiModel) viewStart() string {
	pw := m.panelWidth()
	margin := strings.Repeat(" ", max((m.width-pw)/2, 0))
	block := m.viewPanel(pw) + "\n" + m.viewFooter(pw)
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
	info := ansi.Truncate(panelAccentText.Render("Default")+panelDim.Render(" · "+m.profile.Agent), w, "…")

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
		return lineWithHints(width, statusStyle.Render("working…"), statusStyle.Render("esc take back · stop"))
	}
	identity := m.subject
	if identity == "" {
		identity = "unknown"
	}
	return lineWithHints(width, statusStyle.Render(identity+" · "+m.profile.Server), statusStyle.Render("shift+enter newline"))
}
