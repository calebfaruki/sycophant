// Package config implements profile-based TOML config, credentials, and history
// for the syco CLI.
package config

import (
	"encoding/base64"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/BurntSushi/toml"
)

// Profile holds the resolved configuration for the active profile.
type Profile struct {
	Name   string
	Server string
	Agent  string
}

// configFile is the TOML structure of config.toml.
type configFile struct {
	Default  defaultSection             `toml:"default"`
	Profile  map[string]profileSection  `toml:"profile"`
}

type defaultSection struct {
	Profile string `toml:"profile"`
}

type profileSection struct {
	Server string `toml:"server,omitempty"`
	Agent  string `toml:"agent,omitempty"`
}

// resolveProfileName picks the active profile: flag > env > config default.
func resolveProfileName(cfg *configFile, profileFlag string) string {
	name := cfg.Default.Profile
	if envProfile := os.Getenv("SYCO_PROFILE"); envProfile != "" {
		name = envProfile
	}
	if profileFlag != "" {
		name = profileFlag
	}
	return name
}

// LoadProfile resolves the active profile from the config directory.
// profileFlag and serverFlag are CLI overrides (empty means not set).
func LoadProfile(configDir, profileFlag, serverFlag string) (*Profile, error) {
	cfgPath := filepath.Join(configDir, "config.toml")
	var cfg configFile
	if _, err := toml.DecodeFile(cfgPath, &cfg); err != nil {
		return nil, fmt.Errorf("reading config: %w", err)
	}

	name := resolveProfileName(&cfg, profileFlag)
	if name == "" {
		return nil, fmt.Errorf("no active profile: set [default].profile in config.toml")
	}

	section, ok := cfg.Profile[name]
	if !ok {
		return nil, fmt.Errorf("profile %q not found in config.toml", name)
	}

	// Resolve server: flag > env > profile
	server := section.Server
	if envServer := os.Getenv("SYCO_SERVER"); envServer != "" {
		server = envServer
	}
	if serverFlag != "" {
		server = serverFlag
	}
	if server == "" {
		return nil, fmt.Errorf("server is required in profile %q or via SYCO_SERVER", name)
	}

	// Validate server as host:port
	if _, _, err := net.SplitHostPort(server); err != nil {
		return nil, fmt.Errorf("invalid server %q: must be host:port", server)
	}

	// Resolve agent: env > profile
	agent := section.Agent
	if envAgent := os.Getenv("SYCO_AGENT"); envAgent != "" {
		agent = envAgent
	}

	return &Profile{
		Name:   name,
		Server: server,
		Agent:  agent,
	}, nil
}

// SetAgent writes the agent to the active profile in config.toml.
func SetAgent(configDir, profileFlag, serverFlag, agent string) error {
	cfgPath := filepath.Join(configDir, "config.toml")
	var cfg configFile
	if _, err := toml.DecodeFile(cfgPath, &cfg); err != nil {
		return fmt.Errorf("reading config: %w", err)
	}

	// Resolve active profile name
	name := resolveProfileName(&cfg, profileFlag)

	section, ok := cfg.Profile[name]
	if !ok {
		return fmt.Errorf("profile %q not found in config.toml", name)
	}

	section.Agent = agent
	cfg.Profile[name] = section

	return writeConfig(cfgPath, &cfg)
}

// ListProfiles returns all profile names from config.toml.
func ListProfiles(configDir string) ([]string, error) {
	cfgPath := filepath.Join(configDir, "config.toml")
	var cfg configFile
	if _, err := toml.DecodeFile(cfgPath, &cfg); err != nil {
		return nil, fmt.Errorf("reading config: %w", err)
	}

	names := make([]string, 0, len(cfg.Profile))
	for name := range cfg.Profile {
		names = append(names, name)
	}
	sort.Strings(names)
	return names, nil
}

// UseProfile sets the active profile in [default].profile.
func UseProfile(configDir, name string) error {
	cfgPath := filepath.Join(configDir, "config.toml")
	var cfg configFile
	if _, err := toml.DecodeFile(cfgPath, &cfg); err != nil {
		return fmt.Errorf("reading config: %w", err)
	}

	if _, ok := cfg.Profile[name]; !ok {
		return fmt.Errorf("profile %q not found in config.toml", name)
	}

	cfg.Default.Profile = name
	return writeConfig(cfgPath, &cfg)
}

func writeConfig(path string, cfg *configFile) error {
	f, err := os.Create(path)
	if err != nil {
		return err
	}
	defer f.Close()
	return toml.NewEncoder(f).Encode(cfg)
}

// --- Credentials ---

type credentialsFile map[string]credentialEntry

type credentialEntry struct {
	Token string `toml:"token"`
}

// SaveToken writes a token for the given profile.
func SaveToken(configDir, profile, token string) error {
	credPath := filepath.Join(configDir, "credentials.toml")
	creds := make(credentialsFile)
	// Read existing if present
	if data, err := os.ReadFile(credPath); err == nil {
		toml.Decode(string(data), &creds)
	}
	creds[profile] = credentialEntry{Token: token}

	f, err := os.Create(credPath)
	if err != nil {
		return err
	}
	defer f.Close()
	return toml.NewEncoder(f).Encode(creds)
}

// LoadToken reads and validates (expiry only) the token for a profile.
func LoadToken(configDir, profile string) (string, error) {
	credPath := filepath.Join(configDir, "credentials.toml")
	creds := make(credentialsFile)
	if _, err := toml.DecodeFile(credPath, &creds); err != nil {
		return "", fmt.Errorf("reading credentials: %w", err)
	}

	entry, ok := creds[profile]
	if !ok || entry.Token == "" {
		return "", fmt.Errorf("no token for profile %q; run syco login", profile)
	}

	// Validate expiry
	if err := checkTokenExpiry(entry.Token); err != nil {
		return "", err
	}

	return entry.Token, nil
}

// DeleteToken removes the token for a profile.
func DeleteToken(configDir, profile string) error {
	credPath := filepath.Join(configDir, "credentials.toml")
	creds := make(credentialsFile)
	if data, err := os.ReadFile(credPath); err == nil {
		toml.Decode(string(data), &creds)
	}
	delete(creds, profile)

	f, err := os.Create(credPath)
	if err != nil {
		return err
	}
	defer f.Close()
	return toml.NewEncoder(f).Encode(creds)
}

// decodeJWTPayload extracts and base64-decodes the payload section of a JWT.
// No signature validation — the relay does that.
func decodeJWTPayload(token string) ([]byte, error) {
	parts := strings.Split(token, ".")
	if len(parts) < 2 {
		return nil, fmt.Errorf("invalid token")
	}
	payload := parts[1]
	switch len(payload) % 4 {
	case 2:
		payload += "=="
	case 3:
		payload += "="
	}
	return base64.URLEncoding.DecodeString(payload)
}

// checkTokenExpiry decodes the JWT payload and checks the exp claim.
func checkTokenExpiry(token string) error {
	decoded, err := decodeJWTPayload(token)
	if err != nil {
		return fmt.Errorf("session expired, run syco login")
	}
	var claims struct {
		Exp int64 `json:"exp"`
	}
	if err := json.Unmarshal(decoded, &claims); err != nil {
		return fmt.Errorf("session expired, run syco login")
	}
	if claims.Exp > 0 && time.Unix(claims.Exp, 0).After(time.Now()) {
		return nil
	}
	return fmt.Errorf("session expired, run syco login")
}

// TokenSubject extracts the sub claim from a JWT (no signature validation).
func TokenSubject(token string) (string, error) {
	decoded, err := decodeJWTPayload(token)
	if err != nil {
		return "", fmt.Errorf("invalid token payload")
	}
	var claims struct {
		Sub string `json:"sub"`
	}
	if err := json.Unmarshal(decoded, &claims); err != nil {
		return "", fmt.Errorf("invalid token claims")
	}
	return claims.Sub, nil
}

// TokenEmail extracts the email claim from a JWT (no signature validation).
// Returns empty string if absent.
func TokenEmail(token string) string {
	decoded, err := decodeJWTPayload(token)
	if err != nil {
		return ""
	}
	var claims struct {
		Email string `json:"email"`
	}
	if err := json.Unmarshal(decoded, &claims); err != nil {
		return ""
	}
	return claims.Email
}

// TokenExpiry extracts the exp claim from a JWT and returns it as a time.
func TokenExpiry(token string) (time.Time, error) {
	decoded, err := decodeJWTPayload(token)
	if err != nil {
		return time.Time{}, fmt.Errorf("invalid token payload")
	}
	var claims struct {
		Exp int64 `json:"exp"`
	}
	if err := json.Unmarshal(decoded, &claims); err != nil {
		return time.Time{}, fmt.Errorf("invalid token claims")
	}
	return time.Unix(claims.Exp, 0), nil
}

// --- History ---

// HistoryEntry represents one line of the history file.
type HistoryEntry struct {
	ConversationID string
	Agent          string
	Timestamp      string
}

// HistoryLastForAgent returns the most recent entry for the given agent.
func HistoryLastForAgent(configDir, agent string) (*HistoryEntry, error) {
	entries, err := readHistory(configDir)
	if err != nil {
		return nil, err
	}
	var last *HistoryEntry
	for i := range entries {
		if entries[i].Agent == agent {
			last = &entries[i]
		}
	}
	if last == nil {
		return nil, fmt.Errorf("no previous conversation for agent %q", agent)
	}
	return last, nil
}

// AppendHistory writes a new entry to the history file.
func AppendHistory(configDir, conversationID, agent string) error {
	histPath := filepath.Join(configDir, "history")
	f, err := os.OpenFile(histPath, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0644)
	if err != nil {
		return err
	}
	defer f.Close()
	ts := time.Now().UTC().Format(time.RFC3339)
	_, err = fmt.Fprintf(f, "%s\t%s\t%s\n", conversationID, agent, ts)
	return err
}

func readHistory(configDir string) ([]HistoryEntry, error) {
	histPath := filepath.Join(configDir, "history")
	data, err := os.ReadFile(histPath)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, fmt.Errorf("no history file")
		}
		return nil, err
	}
	var entries []HistoryEntry
	for _, line := range strings.Split(string(data), "\n") {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}
		parts := strings.SplitN(line, "\t", 3)
		if len(parts) != 3 {
			continue
		}
		entries = append(entries, HistoryEntry{
			ConversationID: parts[0],
			Agent:          parts[1],
			Timestamp:      parts[2],
		})
	}
	return entries, nil
}

// ConfigDir returns the config directory, respecting SYCO_CONFIG_DIR.
func ConfigDir() string {
	if dir := os.Getenv("SYCO_CONFIG_DIR"); dir != "" {
		return dir
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return filepath.Join(".", ".config", "syco")
	}
	return filepath.Join(home, ".config", "syco")
}
