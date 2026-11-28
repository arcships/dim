package main

import (
	"bufio"
	"context"
	"crypto/ed25519"
	"crypto/tls"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"time"

	"golang.org/x/term"
)

func tokenInput() (string, error) {
	fmt.Fprint(os.Stderr, "Sharing token: ")
	if term.IsTerminal(int(os.Stdin.Fd())) {
		bytes, err := term.ReadPassword(int(os.Stdin.Fd()))
		fmt.Fprintln(os.Stderr)
		return string(bytes), err
	}
	var token strings.Builder
	var byteIn [1]byte
	for token.Len() <= 128 {
		_, err := io.ReadFull(os.Stdin, byteIn[:])
		if errors.Is(err, io.EOF) || byteIn[0] == '\n' {
			break
		}
		if err != nil {
			return "", err
		}
		token.WriteByte(byteIn[0])
	}
	return token.String(), nil
}

var errPairingRequired = errors.New("Enter the sharing token shown on the other computer")

type tunnelRequest struct {
	Token    string `json:"token"`
	Remember bool   `json:"remember"`
}
type tunnelReady struct {
	targetInfo
	Endpoint string `json:"endpoint"`
	Identity string `json:"identity"`
	Address  string `json:"address"`
}

func tunnel(ctx context.Context, dir, address string) error {
	input := bufio.NewReader(os.Stdin)
	line, err := input.ReadString('\n')
	if err != nil {
		return err
	}
	var request tunnelRequest
	if err := json.Unmarshal([]byte(line), &request); err != nil {
		return err
	}
	ctx, stop := context.WithCancel(ctx)
	defer stop()
	go func() { _, _ = io.Copy(io.Discard, input); stop() }()
	err = connect(ctx, dir, address, request.Remember, request.Token != "", nil, &request)
	if errors.Is(err, errPairingRequired) || errors.Is(err, errCredential) {
		return json.NewEncoder(os.Stdout).Encode(struct {
			Pairing string `json:"pairing"`
		}{err.Error()})
	}
	return err
}
func connect(ctx context.Context, dir, address string, saveToken, fresh bool, args []string, request *tunnelRequest) error {
	known, err := readKnown(dir)
	if err != nil {
		return err
	}
	var identity string
	var routes []string
	for id, computer := range known.Tokens {
		if computer.Name == address && known.Routes[address] == "" {
			if identity != "" {
				return errors.New("multiple saved computers have this name; enter an address")
			}
			identity = id
		}
	}
	if identity != "" {
		for route, id := range known.Routes {
			if id == identity {
				routes = append(routes, route)
			}
		}
		sort.Strings(routes)
		if len(routes) == 0 {
			return errors.New("no saved address for this computer")
		}
		address = routes[0]
	}
	if _, _, err := net.SplitHostPort(address); err != nil {
		address = net.JoinHostPort(strings.Trim(address, "[]"), "7443")
	}
	if len(routes) == 0 {
		routes = []string{address}
		if id := known.Routes[address]; id != "" {
			for other, owner := range known.Routes {
				if other != address && owner == id {
					routes = append(routes, other)
				}
			}
			sort.Strings(routes[1:])
		}
	}
	var token string
	var cred credential
	confirmed := identity
	if confirmed == "" {
		confirmed = known.Routes[address]
	}
	if !fresh {
		token = known.Tokens[confirmed].Token
	}
	if token == "" && !fresh && confirmed == "" && len(known.Tokens) > 0 {
		raw, err := (&net.Dialer{Timeout: 8 * time.Second}).DialContext(ctx, "tcp", address)
		if err != nil {
			return err
		}
		probe := tls.Client(raw, &tls.Config{MinVersion: tls.VersionTLS13, NextProtos: []string{protocol}, InsecureSkipVerify: true, VerifyConnection: func(state tls.ConnectionState) error {
			if len(state.PeerCertificates) == 1 {
				if key, ok := state.PeerCertificates[0].PublicKey.(ed25519.PublicKey); ok {
					token = known.Tokens[encoding.EncodeToString(fingerprint(key))].Token
				}
			}
			return errors.New("identity lookup complete")
		}})
		probeCtx, cancel := context.WithTimeout(ctx, 8*time.Second)
		_ = probe.HandshakeContext(probeCtx)
		cancel()
		raw.Close()
	}
	if request != nil && fresh {
		token = request.Token
	}
	if token == "" && request != nil {
		return errPairingRequired
	}
	if token == "" {
		token, err = tokenInput()
		if err != nil {
			return err
		}
	}
	cred, err = parseCredential(token)
	if err != nil {
		return err
	}
	if confirmed != "" && confirmed != encoding.EncodeToString(cred.pin) && !fresh {
		return errors.New("the computer at this address changed; use --pair to confirm its new token")
	}
	tlsConn, info, address, err := dialTarget(ctx, routes, cred)
	if err != nil {
		return err
	}
	defer tlsConn.Close()
	if saveToken {
		if err = remember(dir, address, token, info.Name, cred); err != nil {
			return err
		}
	}
	tmp, err := os.MkdirTemp("", "dim-link-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(tmp)
	endpoint := filepath.Join(tmp, "rpc.sock")
	if runtime.GOOS == "windows" {
		suffix, err := randomBytes(24)
		if err != nil {
			return err
		}
		endpoint = `\\.\pipe\dim-link-` + encoding.EncodeToString(suffix)
	}
	ready := filepath.Join(tmp, "ready")
	link, stop := context.WithCancel(ctx)
	done := make(chan error, 1)
	go func() { done <- runPeer(link, tlsConn, sibling("dim-terminal-transport"), "connect", endpoint, ready) }()
	defer func() { stop(); tlsConn.Close(); <-done }()
	deadline := time.NewTimer(15 * time.Second)
	defer deadline.Stop()
	tick := time.NewTicker(30 * time.Millisecond)
	defer tick.Stop()
	for {
		if _, err = os.Stat(ready); err == nil {
			break
		}
		select {
		case err := <-done:
			done <- err
			return fmt.Errorf("terminal transport unavailable: %v", err)
		case <-deadline.C:
			return errors.New("terminal transport startup timed out")
		case <-tick.C:
		}
	}
	id := encoding.EncodeToString(cred.pin)
	if request != nil {
		if err := json.NewEncoder(os.Stdout).Encode(tunnelReady{info, endpoint, id, address}); err != nil {
			return err
		}
		select {
		case <-ctx.Done():
			return nil
		case err := <-done:
			done <- err
			return err
		}
	}
	cmd := exec.CommandContext(ctx, sibling("dimcode"), args...)
	cmd.Env = append(os.Environ(), "DIMCODE_TERMINAL_PROXY="+endpoint, "DIMCODE_TERMINAL_WORKSPACE="+info.WorkingDirectory, "DIMCODE_TERMINAL_NAME="+info.Name, "DIMCODE_TERMINAL_ID="+id, "DIMCODE_TERMINAL_ADDRESS="+address)
	cmd.Stdin, cmd.Stdout, cmd.Stderr = os.Stdin, os.Stdout, os.Stderr
	return cmd.Run()
}

func dialTarget(ctx context.Context, routes []string, cred credential) (net.Conn, targetInfo, string, error) {
	cfg := &tls.Config{MinVersion: tls.VersionTLS13, NextProtos: []string{protocol}, InsecureSkipVerify: true, VerifyConnection: cred.verify}
	var failures []error
	for _, route := range routes {
		if ctx.Err() != nil {
			return nil, targetInfo{}, "", ctx.Err()
		}
		conn, info, err := dialRoute(ctx, route, cfg, cred)
		if err == nil {
			return conn, info, route, nil
		}
		failures = append(failures, err)
	}
	return nil, targetInfo{}, "", errors.Join(failures...)
}

func dialRoute(ctx context.Context, route string, cfg *tls.Config, cred credential) (net.Conn, targetInfo, error) {
	raw, err := (&net.Dialer{Timeout: 8 * time.Second}).DialContext(ctx, "tcp", route)
	if err != nil {
		return nil, targetInfo{}, err
	}
	conn, err := authenticate(ctx, raw, cfg, false, cred.secret)
	if err != nil {
		raw.Close()
		return nil, targetInfo{}, fmt.Errorf("sharing authentication failed (use --pair after a token reset): %w", err)
	}
	var info targetInfo
	if err = conn.SetReadDeadline(time.Now().Add(5 * time.Second)); err == nil {
		info, err = readTarget(conn)
	}
	if err == nil {
		err = conn.SetReadDeadline(time.Time{})
	}
	if err != nil {
		raw.Close()
		return nil, targetInfo{}, err
	}
	return conn, info, nil
}
