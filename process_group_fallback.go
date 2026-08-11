//go:build aix || js || plan9 || wasip1 || windows

package main

import "os/exec"

func configureProcessGroup(cmd *exec.Cmd) {}

func cancelProcessGroup(cmd *exec.Cmd) error {
	if cmd.Process == nil {
		return nil
	}
	return cmd.Process.Kill()
}
