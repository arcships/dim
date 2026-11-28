package main

import (
	"crypto/ed25519"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/gofrs/flock"
)

type settings struct {
	Name     string   `json:"name"`
	Enabled  bool     `json:"enabled"`
	Identity identity `json:"identity"`
	Revision uint64   `json:"revision"`
}
type savedComputer struct {
	Name  string `json:"name"`
	Token string `json:"token"`
}
type known struct {
	Tokens map[string]savedComputer `json:"tokens"`
	Routes map[string]string        `json:"routes"`
}

func load(path string, dst any) error {
	info, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if !info.Mode().IsRegular() || (runtime.GOOS != "windows" && info.Mode().Perm()&0077 != 0) {
		return errors.New("sharing data must be a private regular file")
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	return json.Unmarshal(data, dst)
}
func save(path string, data any) error {
	f, err := os.CreateTemp(filepath.Dir(path), ".sharing-*")
	if err != nil {
		return err
	}
	defer os.Remove(f.Name())
	err = json.NewEncoder(f).Encode(data)
	if err == nil {
		err = f.Sync()
	}
	ce := f.Close()
	if err != nil {
		return err
	}
	if ce != nil {
		return ce
	}
	return os.Rename(f.Name(), path)
}
func readSettings(dir string) (settings, error) {
	var s settings
	err := load(filepath.Join(dir, "sharing.json"), &s)
	if errors.Is(err, os.ErrNotExist) {
		s.Name, err = os.Hostname()
		if err == nil && !validName(s.Name) {
			s.Name = strings.SplitN(s.Name, ".", 2)[0]
			if !validName(s.Name) {
				s.Name = "Dim"
			}
		}
		return s, err
	}
	if err == nil && (len(s.Identity.Seed) != ed25519.SeedSize || len(s.Identity.Secret) != secretSize) {
		err = errCredential
	}
	return s, err
}
func validName(name string) bool {
	return name != "" && len(name) <= 63 && utf8.ValidString(name) && strings.IndexFunc(name, unicode.IsControl) < 0
}
func mutate(dir, op, name string) (settings, error) {
	lock := flock.New(filepath.Join(dir, "settings.lock"))
	if err := lock.Lock(); err != nil {
		return settings{}, err
	}
	defer lock.Unlock()
	s, err := readSettings(dir)
	if err != nil {
		return s, err
	}
	if s.Identity.Seed == nil {
		s.Identity, err = newIdentity()
		if err != nil {
			return s, err
		}
	}
	switch op {
	case "enable":
		s.Enabled = true
	case "disable":
		s.Enabled = false
	case "rename":
		name = strings.TrimSpace(name)
		if !validName(name) {
			return s, errors.New("name must be 1 to 63 UTF-8 bytes without control characters")
		}
		s.Name = name
	case "reset":
		s.Identity.Secret, err = randomBytes(secretSize)
		if err != nil {
			return s, err
		}
	default:
		return s, fmt.Errorf("unknown sharing setting %q", op)
	}
	s.Revision++
	return s, save(filepath.Join(dir, "sharing.json"), s)
}
func readKnown(dir string) (known, error) {
	k := known{Tokens: map[string]savedComputer{}, Routes: map[string]string{}}
	err := load(filepath.Join(dir, "known.json"), &k)
	if errors.Is(err, os.ErrNotExist) {
		err = nil
	}
	if err == nil && (k.Tokens == nil || k.Routes == nil) {
		err = errors.New("invalid remembered computers")
	}
	return k, err
}
func remember(dir, address, token, name string, c credential) error {
	lock := flock.New(filepath.Join(dir, "known.lock"))
	if err := lock.Lock(); err != nil {
		return err
	}
	defer lock.Unlock()
	k, err := readKnown(dir)
	if err != nil {
		return err
	}
	id := encoding.EncodeToString(c.pin)
	k.Tokens[id] = savedComputer{name, token}
	k.Routes[address] = id
	return save(filepath.Join(dir, "known.json"), k)
}
