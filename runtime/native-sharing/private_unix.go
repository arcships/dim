//go:build !windows

package main

import (
	"errors"
	"os"
	"syscall"
)

func secureDirectory(path string) error {
	info, err := os.Lstat(path)
	if err != nil {
		return err
	}
	owner, ok := info.Sys().(*syscall.Stat_t)
	if !ok || !info.IsDir() || info.Mode().Perm()&0077 != 0 || int(owner.Uid) != os.Geteuid() {
		return errors.New("sharing directory must be private and owned by the current user")
	}
	return nil
}
