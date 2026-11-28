package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"runtime"
	"syscall"

	"github.com/arcships/rutis-network/discovery/mdns"
	"github.com/arcships/rutis-network/link/tcp"
	"github.com/arcships/rutis/go/rutis/advanced"
)

func sibling(name string) string {
	path, err := os.Executable()
	if err != nil {
		panic(err)
	}
	if runtime.GOOS == "windows" {
		name += ".exe"
	}
	return filepath.Join(filepath.Dir(path), name)
}
func main() {
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()
	if err := run(ctx, os.Args[1:]); err != nil {
		var exit *exec.ExitError
		if errors.As(err, &exit) && exit.ExitCode() > 0 {
			os.Exit(exit.ExitCode())
		}
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
func run(ctx context.Context, args []string) error {
	if len(args) > 0 && args[0] != "--home" {
		return advanced.ServeArgs(args, mdns.Plugin, tcp.Dial, tcp.Listen)
	}
	if len(args) < 3 {
		return errors.New("usage: dim-sharing --home DIR status|enable|disable|rename NAME|reset|discover|connect ADDRESS [--pair] [--no-remember] [-- CLI_ARGS...]")
	}
	dir := filepath.Join(args[1], "sharing")
	op := args[2]
	rest := args[3:]
	if err := os.MkdirAll(dir, 0700); err != nil {
		return err
	}
	if err := secureDirectory(dir); err != nil {
		return err
	}
	switch op {
	case "serve":
		if len(rest) != 1 {
			return errors.New("missing App Server endpoint")
		}
		ctx, stop := context.WithCancel(ctx)
		defer stop()
		go func() { _, _ = io.Copy(io.Discard, os.Stdin); stop() }()
		return sharing(ctx, dir, rest[0])
	case "known":
		k, err := readKnown(dir)
		if err != nil {
			return err
		}
		return json.NewEncoder(os.Stdout).Encode(savedDevices(k))
	case "discover":
		return browse(ctx, dir)
	case "tunnel":
		if len(rest) != 1 {
			return errors.New("tunnel needs an address")
		}
		return tunnel(ctx, dir, rest[0])
	case "connect":
		if len(rest) == 0 {
			return errors.New("connect needs an address")
		}
		address := rest[0]
		args := rest[1:]
		remember, fresh := true, false
		pick := false
	flags:
		for len(args) > 0 {
			switch args[0] {
			case "--pick-sessions":
				pick = true
			case "--pair":
				fresh = true
			case "--no-remember":
				remember = false
			case "--":
				args = args[1:]
				break flags
			default:
				return errors.New("put native CLI arguments after --")
			}
			args = args[1:]
		}
		if pick {
			os.Setenv("DIMCODE_TERMINAL_PICK", "1")
		}
		return connect(ctx, dir, address, remember, fresh, append([]string{"--home", filepath.Dir(filepath.Dir(dir))}, args...), nil)
	case "status", "enable", "disable", "rename", "reset":
		var s settings
		var err error
		if op == "status" {
			s, err = readSettings(dir)
		} else {
			name := ""
			if op == "rename" {
				if len(rest) != 1 {
					return errors.New("rename needs a computer name")
				}
				name = rest[0]
			}
			s, err = mutate(dir, op, name)
		}
		if err != nil {
			return err
		}
		st, err := localStatus(dir, s, op != "status")
		if err != nil {
			return err
		}
		return json.NewEncoder(os.Stdout).Encode(st)
	default:
		return fmt.Errorf("unknown sharing operation %q", op)
	}
}
