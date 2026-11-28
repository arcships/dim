package main

import (
	"bufio"
	"bytes"
	"context"
	"crypto/subtle"
	"crypto/tls"
	"encoding/binary"
	"encoding/json"
	"errors"
	"io"
	"net"
	"os"
	"os/exec"
	"time"

	"github.com/arcships/rutis-network/link/tcp"
)

const protocol = "dim-terminal/1"
const maxFrame = 16 << 20

type targetInfo struct {
	Name             string `json:"name"`
	WorkingDirectory string `json:"workingDirectory"`
}

func (info targetInfo) write(conn net.Conn) error {
	data, err := json.Marshal(info)
	if err != nil {
		return err
	}
	if len(data) > 16384 {
		return errors.New("target information is too large")
	}
	_, err = conn.Write(append(binary.BigEndian.AppendUint16(nil, uint16(len(data))), data...))
	return err
}
func readTarget(conn net.Conn) (targetInfo, error) {
	var info targetInfo
	var length uint16
	if err := binary.Read(conn, binary.BigEndian, &length); err != nil {
		return info, err
	}
	if length == 0 || length > 16384 {
		return info, errors.New("invalid target information length")
	}
	data := make([]byte, length)
	if _, err := io.ReadFull(conn, data); err != nil {
		return info, err
	}
	if err := json.Unmarshal(data, &info); err != nil {
		return info, err
	}
	if !validName(info.Name) || info.WorkingDirectory == "" {
		return info, errors.New("invalid target information")
	}
	return info, nil
}

func authenticate(ctx context.Context, raw net.Conn, cfg *tls.Config, server bool, secret []byte) (net.Conn, error) {
	if err := raw.SetDeadline(time.Now().Add(10 * time.Second)); err != nil {
		return nil, err
	}
	var conn *tls.Conn
	if server {
		conn = tls.Server(raw, cfg)
	} else {
		conn = tls.Client(raw, cfg)
	}
	if err := conn.HandshakeContext(ctx); err != nil {
		return nil, err
	}
	if conn.ConnectionState().NegotiatedProtocol != protocol {
		return nil, errors.New("incompatible sharing protocol")
	}
	if server {
		var offered [secretSize]byte
		if _, err := io.ReadFull(conn, offered[:]); err != nil {
			return nil, err
		}
		if subtle.ConstantTimeCompare(secret, offered[:]) != 1 {
			_, _ = conn.Write([]byte{0})
			return nil, errCredential
		}
		if _, err := conn.Write([]byte{1}); err != nil {
			return nil, err
		}
	} else {
		if _, err := conn.Write(secret); err != nil {
			return nil, err
		}
		var ack [1]byte
		if _, err := io.ReadFull(conn, ack[:]); err != nil {
			return nil, err
		}
		if ack[0] != 1 {
			return nil, errCredential
		}
	}
	return conn, conn.SetDeadline(time.Time{})
}

// runPeer connects an admitted TCP link to a dedicated rutis kernel. The kernel owns all
// RPC streams for this link and exits with it.
func runPeer(parent context.Context, conn net.Conn, executable string, args ...string) error {
	ctx, cancel := context.WithCancel(parent)
	defer cancel()
	defer conn.Close()
	cmd := exec.CommandContext(ctx, executable, args...)
	cmd.Stderr = os.Stderr
	input, err := cmd.StdinPipe()
	if err != nil {
		return err
	}
	output, err := cmd.StdoutPipe()
	if err != nil {
		return err
	}
	if err = cmd.Start(); err != nil {
		return err
	}
	done, err := tcp.Open(ctx, conn, tcp.Sessions{Open: func(_ context.Context, far tcp.Far) (tcp.Near, error) {
		go func() {
			reader := bufio.NewScanner(output)
			reader.Buffer(make([]byte, 8192), maxFrame+1)
			for reader.Scan() {
				if err := far.Send(reader.Bytes()); err != nil {
					break
				}
			}
			far.Close(tcp.CloseReason{Kind: "done"})
		}()
		return tcp.Near{Receive: func(_ context.Context, frame []byte) error {
			if bytes.IndexByte(frame, '\n') >= 0 {
				return errors.New("invalid rutis frame")
			}
			_, err := input.Write(append(frame, '\n'))
			return err
		}, End: func(context.Context, tcp.CloseReason) error { return input.Close() }}, nil
	}})
	if err == nil {
		err = <-done
	}
	_ = input.Close()
	waited := make(chan error, 1)
	go func() { waited <- cmd.Wait() }()
	select {
	case childErr := <-waited:
		return errors.Join(err, childErr)
	case <-time.After(time.Second):
		cancel()
		<-waited
		return err
	}
}
