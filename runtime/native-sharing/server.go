package main

import (
	"bytes"
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"github.com/fsnotify/fsnotify"
	"net"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"sync"
	"time"

	"github.com/gofrs/flock"
)

type status struct {
	Name      string   `json:"name"`
	Enabled   bool     `json:"enabled"`
	Token     string   `json:"token,omitempty"`
	Addresses []string `json:"addresses"`
	Revision  uint64   `json:"revision"`
	Error     string   `json:"error,omitempty"`
}

func addresses(l net.Listener) []string {
	bound := l.Addr().(*net.TCPAddr)
	if !bound.IP.IsUnspecified() {
		return []string{bound.String()}
	}
	result := []string{}
	ifaces, _ := net.Interfaces()
	for _, i := range ifaces {
		if i.Flags&net.FlagUp == 0 || i.Flags&net.FlagLoopback != 0 {
			continue
		}
		addrs, _ := i.Addrs()
		for _, a := range addrs {
			ip, _, err := net.ParseCIDR(a.String())
			if err == nil && ip.IsGlobalUnicast() {
				result = append(result, net.JoinHostPort(ip.String(), strconv.Itoa(bound.Port)))
			}
		}
	}
	return result
}
func sharing(ctx context.Context, dir, endpoint string) error {
	owner := flock.New(filepath.Join(dir, "serving.lock"))
	ok, err := owner.TryLock()
	if err != nil {
		return err
	}
	if !ok {
		return errors.New("sharing supervisor already running")
	}
	defer owner.Unlock()
	statusFile := filepath.Join(dir, "listener.json")
	defer os.Remove(statusFile)
	watcher, err := fsnotify.NewWatcher()
	if err != nil {
		return err
	}
	defer watcher.Close()
	if err = watcher.Add(dir); err != nil {
		return err
	}
	var active, announce context.CancelFunc
	var peersDone <-chan struct{}
	var announcements sync.WaitGroup
	var listener net.Listener
	stopAnnouncement := func() {
		if announce != nil {
			announce()
			announcements.Wait()
			announce = nil
		}
	}
	stop := func() {
		if active != nil {
			active()
			<-peersDone
			active = nil
			listener = nil
		}
		stopAnnouncement()
	}
	defer stop()
	var applied settings
	var previous status
	refresh := time.NewTicker(15 * time.Second)
	defer refresh.Stop()
	for {
		s, err := readSettings(dir)
		if err != nil {
			return err
		}
		st := status{Name: s.Name, Enabled: s.Enabled, Revision: s.Revision, Addresses: []string{}}
		listen := os.Getenv("DIMCODE_SHARING_LISTEN")
		if listen == "" {
			listen = ":7443"
		}
		if listener != nil {
			listen = listener.Addr().String()
		}
		if !s.Enabled || !bytes.Equal(applied.Identity.Secret, s.Identity.Secret) {
			stop()
		}
		if s.Enabled && listener == nil {
			cert, err := s.Identity.certificate()
			if err != nil {
				return err
			}
			l, err := net.Listen("tcp", listen)
			if err != nil {
				st.Error = err.Error()
			} else {
				var generation context.Context
				generation, active = context.WithCancel(context.WithoutCancel(ctx))
				listener = l
				done := make(chan struct{})
				peersDone = done
				go func() {
					defer close(done)
					acceptLinks(generation, l, &tls.Config{Certificates: []tls.Certificate{cert}, MinVersion: tls.VersionTLS13, NextProtos: []string{protocol}}, s.Identity.Secret, endpoint, dir)
				}()
			}
		}
		if listener != nil {
			st.Addresses = addresses(listener)
			if announce == nil || applied.Name != s.Name || !slices.Equal(st.Addresses, previous.Addresses) {
				stopAnnouncement()
				var generation context.Context
				generation, announce = context.WithCancel(ctx)
				bound := listener.Addr().(*net.TCPAddr)
				announcements.Go(func() { advertise(generation, s.Name, bound) })
			}
		}
		if previous.Name == "" || previous.Revision != st.Revision || !slices.Equal(st.Addresses, previous.Addresses) || previous.Error != st.Error {
			if err = save(statusFile, st); err != nil {
				return err
			}
		}
		applied = s
		previous = st
	waiting:
		for {
			select {
			case <-ctx.Done():
				if listener != nil {
					listener.Close()
					select {
					case <-peersDone:
					case <-time.After(10 * time.Second):
					}
				}
				return nil
			case err := <-watcher.Errors:
				return err
			case event := <-watcher.Events:
				if filepath.Base(event.Name) == "sharing.json" {
					break waiting
				}
			case <-refresh.C:
				if s.Enabled {
					break waiting
				}
			}
		}
	}

}
func acceptLinks(ctx context.Context, l net.Listener, cfg *tls.Config, secret []byte, endpoint, dir string) {
	defer l.Close()
	finished := context.AfterFunc(ctx, func() { l.Close() })
	defer finished()
	var links sync.WaitGroup
	defer links.Wait()
	slots := make(chan struct{}, 32)
	for {
		conn, err := l.Accept()
		if err != nil {
			return
		}
		select {
		case slots <- struct{}{}:
		default:
			conn.Close()
			continue
		}
		links.Go(func() {
			defer func() { <-slots }()
			defer conn.Close()
			stop := context.AfterFunc(ctx, func() { conn.Close() })
			defer stop()
			admitted, err := authenticate(ctx, conn, cfg, true, secret)
			if err != nil {
				return
			}
			current, err := readSettings(dir)
			if err != nil || !validName(current.Name) {
				return
			}
			cwd, err := os.Getwd()
			if err != nil {
				return
			}
			if err = (targetInfo{current.Name, cwd}).write(admitted); err != nil {
				return
			}
			_ = runPeer(ctx, admitted, sibling("dim-terminal-transport"), "serve", endpoint)
		})
	}
}
func localStatus(dir string, s settings, wait bool) (status, error) {
	st := status{Name: s.Name, Enabled: s.Enabled, Revision: s.Revision, Addresses: []string{}}
	deadline := time.Now().Add(4 * time.Second)
	for {
		var live status
		err := load(filepath.Join(dir, "listener.json"), &live)
		applied := err == nil && live.Revision == s.Revision
		if applied || wait && !s.Enabled {
			lock := flock.New(filepath.Join(dir, "serving.lock"))
			free, err := lock.TryLock()
			if err != nil {
				return st, err
			}
			if free {
				lock.Unlock()
				if !s.Enabled {
					break
				}
			} else if applied {
				st = live
				break
			}
		}
		if !wait || time.Now().After(deadline) {
			if s.Enabled || wait {
				st.Error = "sharing supervisor is unavailable"
			}
			break
		}
		time.Sleep(40 * time.Millisecond)
	}
	if s.Enabled && len(s.Identity.Secret) > 0 {
		st.Token = s.Identity.token()
	}
	if st.Error != "" && wait {
		return st, fmt.Errorf("%s", st.Error)
	}
	return st, nil
}
