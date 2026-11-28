package main

import (
	"bufio"
	"context"
	"crypto/tls"
	"errors"
	"net"
	"path/filepath"
	"testing"
)

func TestPairingSurvivesRouteAndNameChangesButNotTokenReset(t *testing.T) {
	server, client := t.TempDir(), t.TempDir()
	initial, err := readSettings(server)
	if err != nil || initial.Enabled {
		t.Fatal("a new installation must be off", err)
	}
	before, err := mutate(server, "enable", "")
	if err != nil {
		t.Fatal(err)
	}
	original := before.Identity.token()
	proof, err := parseCredential(original)
	if err != nil {
		t.Fatal(err)
	}
	dial := func(server identity, offered credential) error {
		cert, err := server.certificate()
		if err != nil {
			t.Fatal(err)
		}
		a, b := net.Pipe()
		ctx := t.Context()
		go func() {
			defer b.Close()
			admitted, err := authenticate(ctx, b, &tls.Config{Certificates: []tls.Certificate{cert}, MinVersion: tls.VersionTLS13, NextProtos: []string{protocol}}, true, server.Secret)
			if err == nil {
				var end [1]byte
				_, _ = admitted.Read(end[:])
			}
		}()
		defer a.Close()
		_, err = authenticate(ctx, a, &tls.Config{MinVersion: tls.VersionTLS13, NextProtos: []string{protocol}, InsecureSkipVerify: true, VerifyConnection: offered.verify}, false, offered.secret)
		return err
	}
	if err = dial(before.Identity, proof); err != nil {
		t.Fatal(err)
	}
	for _, route := range []string{"office.local:7443", "office.test:7443"} {
		if err = remember(client, route, original, "Office", proof); err != nil {
			t.Fatal(err)
		}
	}
	saved, err := readKnown(client)
	if err != nil || len(saved.Tokens) != 1 || len(saved.Routes) != 2 {
		t.Fatal("two verified routes must share one credential", err)
	}
	renamed, err := mutate(server, "rename", "Desk")
	if err != nil {
		t.Fatal(err)
	}
	if renamed.Identity.token() != original {
		t.Fatal("renaming changed the credential")
	}
	replacement, err := newIdentity()
	if err != nil {
		t.Fatal(err)
	}
	if err = dial(replacement, proof); err == nil || errors.Is(err, errCredential) {
		t.Fatal("replacement at the same address must fail certificate verification")
	}
	reset, err := mutate(server, "reset", "")
	if err != nil {
		t.Fatal(err)
	}
	if err = dial(reset.Identity, proof); !errors.Is(err, errCredential) {
		t.Fatal("old secret must fail after reset", err)
	}
	current, err := parseCredential(reset.Identity.token())
	if err != nil {
		t.Fatal(err)
	}
	if err = dial(reset.Identity, current); err != nil {
		t.Fatal(err)
	}
}

func TestSavedRoutesAuthenticateTheComputerBeforeSelectingAnAddress(t *testing.T) {
	drive := func(id identity) string {
		t.Helper()
		cert, err := id.certificate()
		if err != nil {
			t.Fatal(err)
		}
		listener, err := net.Listen("tcp", "127.0.0.1:0")
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { listener.Close() })
		go func() {
			raw, err := listener.Accept()
			if err != nil {
				return
			}
			defer raw.Close()
			conn, err := authenticate(t.Context(), raw, &tls.Config{Certificates: []tls.Certificate{cert}, MinVersion: tls.VersionTLS13, NextProtos: []string{protocol}}, true, id.Secret)
			if err == nil {
				_ = (targetInfo{Name: "Fixture", WorkingDirectory: "/project"}).write(conn)
			}
		}()
		return listener.Addr().String()
	}
	stranger, err := newIdentity()
	if err != nil {
		t.Fatal(err)
	}
	saved, err := newIdentity()
	if err != nil {
		t.Fatal(err)
	}
	cred, err := parseCredential(saved.token())
	if err != nil {
		t.Fatal(err)
	}
	first, second := drive(stranger), drive(saved)
	conn, info, route, err := dialTarget(t.Context(), []string{first, second}, cred)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	if route != second || info.Name != "Fixture" {
		t.Fatal("the second route must serve the saved identity")
	}
}

func TestSupervisorRevokesALiveLinkAndWithdrawsTheListener(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("DIMCODE_SHARING_LISTEN", "127.0.0.1:0")
	ctx, stop := context.WithCancel(t.Context())
	owner := make(chan error, 1)
	go func() { owner <- sharing(ctx, dir, filepath.Join(dir, "daemon.sock")) }()
	defer func() {
		stop()
		if err := <-owner; err != nil {
			t.Error(err)
		}
	}()
	s, err := mutate(dir, "enable", "")
	if err != nil {
		t.Fatal(err)
	}
	st, err := localStatus(dir, s, true)
	if err != nil {
		t.Fatal(err)
	}
	endpoint := st.Addresses[0]
	raw, err := net.Dial("tcp", endpoint)
	if err != nil {
		t.Fatal(err)
	}
	defer raw.Close()
	if _, err = raw.Write([]byte{22, 3}); err != nil {
		t.Fatal(err)
	}
	s, err = mutate(dir, "reset", "")
	if err != nil {
		t.Fatal(err)
	}
	if _, err = localStatus(dir, s, true); err != nil {
		t.Fatal(err)
	}
	if _, err = bufio.NewReader(raw).ReadByte(); err == nil {
		t.Fatal("old connection stayed open")
	}
	s, err = mutate(dir, "disable", "")
	if err != nil {
		t.Fatal(err)
	}
	st, err = localStatus(dir, s, true)
	if err != nil || len(st.Addresses) != 0 || st.Token != "" {
		t.Fatal("disabled listener still visible", err)
	}
	if conn, err := net.Dial("tcp", endpoint); err == nil {
		conn.Close()
		t.Fatal("withdrawn listener accepted a connection")
	}
	if err = load(filepath.Join(dir, "sharing.json"), &s); err != nil {
		t.Fatal(err)
	}
	if s.Enabled {
		t.Fatal("disabled state must survive a restart")
	}
}
