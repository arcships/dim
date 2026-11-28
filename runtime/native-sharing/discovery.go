package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"sort"
	"strconv"
	"time"

	"github.com/arcships/rutis-network/discovery/mdns"
)

type advertisement struct {
	Type     string `json:"type"`
	Instance string `json:"instance"`
	Port     int    `json:"port"`
}
type discoveryRow struct {
	ID     string           `json:"id"`
	Name   string           `json:"name"`
	Config discoveryOptions `json:"config"`
}
type discoveryOptions struct {
	Interface string         `json:"interface"`
	Service   string         `json:"service,omitempty"`
	Browse    string         `json:"browse,omitempty"`
	Publish   *advertisement `json:"publish,omitempty"`
}
type goBinaries struct {
	Binaries []string `json:"binaries"`
}
type manifest struct {
	ID       string                `json:"id"`
	Runtimes map[string]goBinaries `json:"runtimes"`
	Rows     []discoveryRow        `json:"rows"`
}

const serviceType = "_dim-alpha._tcp"

func discovery(ctx context.Context, publish *advertisement, bound net.IP, out io.Writer) error {
	tmp, err := os.MkdirTemp("", "dim-mdns-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(tmp)
	ifaces, err := net.Interfaces()
	if err != nil {
		return err
	}
	rows := []discoveryRow{}
	for _, i := range ifaces {
		if i.Flags&(net.FlagUp|net.FlagMulticast) != net.FlagUp|net.FlagMulticast || i.Flags&(net.FlagLoopback|net.FlagPointToPoint) != 0 {
			continue
		}
		matches := false
		addresses, _ := i.Addrs()
		for _, a := range addresses {
			ip, _, err := net.ParseCIDR(a.String())
			if err == nil && (ip.IsGlobalUnicast() || ip.IsLinkLocalUnicast()) && (bound == nil || bound.IsUnspecified() || ip.Equal(bound)) {
				matches = true
				break
			}
		}
		if !matches {
			continue
		}
		opts := discoveryOptions{Interface: i.Name, Publish: publish}
		id := "lan-" + strconv.Itoa(i.Index)
		if publish == nil {
			opts.Browse = serviceType
			opts.Service = id
		}
		rows = append(rows, discoveryRow{id, "go:discover-mdns", opts})
	}
	if len(rows) == 0 {
		if publish == nil {
			return json.NewEncoder(out).Encode([]mdns.Candidate{})
		}
		return nil
	}
	file := filepath.Join(tmp, "rutis.json")
	if err = save(file, manifest{"dim-discovery", map[string]goBinaries{"go": {[]string{sibling("dim-sharing")}}}, rows}); err != nil {
		return err
	}
	mode := "announce"
	if publish == nil {
		mode = "discover"
	}
	args := []string{mode, file}
	if publish == nil {
		for _, row := range rows {
			args = append(args, row.Config.Service)
		}
	}
	cmd := exec.CommandContext(ctx, sibling("dim-terminal-transport"), args...)
	cmd.Stdout = out
	cmd.Stderr = os.Stderr
	cmd.Dir = tmp
	input, err := cmd.StdinPipe()
	if err != nil {
		return err
	}
	defer input.Close()
	cmd.Cancel = input.Close
	cmd.WaitDelay = 3 * time.Second
	return cmd.Run()
}
func advertise(ctx context.Context, name string, bound *net.TCPAddr) {
	for ctx.Err() == nil {
		if err := discovery(ctx, &advertisement{serviceType, name, bound.Port}, bound.IP, io.Discard); err != nil && ctx.Err() == nil {
			fmt.Fprintln(os.Stderr, "LAN announcement unavailable:", err)
		}
		select {
		case <-ctx.Done():
			return
		case <-time.After(15 * time.Second):
		}
	}
}

type device struct {
	Name      string   `json:"name"`
	Addresses []string `json:"addresses"`
	Verified  bool     `json:"verified"`
}

func savedDevices(k known) []device {
	devices := []device{}
	for id, saved := range k.Tokens {
		d := device{Name: saved.Name, Verified: true}
		for address, owner := range k.Routes {
			if owner == id {
				d.Addresses = append(d.Addresses, address)
			}
		}
		slices.Sort(d.Addresses)
		if len(d.Addresses) > 0 {
			devices = append(devices, d)
		}
	}
	sort.Slice(devices, func(i, j int) bool {
		if devices[i].Name == devices[j].Name {
			return devices[i].Addresses[0] < devices[j].Addresses[0]
		}
		return devices[i].Name < devices[j].Name
	})
	return devices
}

func browse(ctx context.Context, dir string) error {
	k, err := readKnown(dir)
	if err != nil {
		return err
	}
	var observed bytes.Buffer
	window, cancel := context.WithTimeout(ctx, 12*time.Second)
	defer cancel()
	err = discovery(window, nil, nil, &observed)
	if err != nil {
		fmt.Fprintln(os.Stderr, "LAN discovery unavailable:", err)
	}
	var candidates []mdns.Candidate
	if err == nil {
		if err = json.Unmarshal(observed.Bytes(), &candidates); err != nil {
			return err
		}
	}
	devices := savedDevices(k)
	for _, c := range candidates {
		d := device{Name: c.Instance}
		if !validName(d.Name) {
			continue
		}
		for _, address := range c.Addresses {
			ip := net.ParseIP(address)
			if ip == nil {
				continue
			}
			if ip.IsLinkLocalUnicast() && ip.To4() == nil {
				address += "%" + c.Interface
			}
			route := net.JoinHostPort(address, strconv.Itoa(int(c.Port)))
			if k.Routes[route] == "" {
				d.Addresses = append(d.Addresses, route)
			}
		}
		if len(d.Addresses) > 0 {
			devices = append(devices, d)
		}
	}
	return json.NewEncoder(os.Stdout).Encode(devices)
}
