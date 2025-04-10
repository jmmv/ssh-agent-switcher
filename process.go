package main

import (
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

// getProcDir returns the base directory for proc filesystem
// This can be overridden by setting PROCESS_OVERRIDE_PROC_DIR environment variable
// which is useful for testing
func getProcDir() string {
	override := os.Getenv("PROCESS_OVERRIDE_PROC_DIR")
	if override != "" {
		return override
	}
	return "/proc"
}

// hasAttachedPts checks if the sshd process with the given PID has a PTS attached.
//
// The process description in 'ps' is either something like "sshd: user@notty" or "sshd: user@pts/1".
func hasAttachedPts(pid int) bool {
	// Read the process description
	path := fmt.Sprintf("%s/%d/cmdline", getProcDir(), pid)
	name, err := os.ReadFile(path)
	if err != nil {
		return false
	}

	return strings.Contains(string(name), "@pts/")
}

// getSocketOwnerPid returns the PID of the sshd process that owns the socket.
//
// We have a filename like "/tmp/ssh-XYZ/agent.PID", where XYZ is some identifier
// and PID is the process ID of the sshd that created the socket.
func getSocketOwnerPid(socketPath string) (int, error) {
	// Extract the filename part of the path
	socketFilename := filepath.Base(socketPath)
	pidStr := strings.TrimPrefix(socketFilename, "agent.")
	pid, err := strconv.Atoi(pidStr)
	if err != nil {
		return -1, fmt.Errorf("invalid socket path: %s", socketPath)
	}
	return pid, nil
}

// isSSHDProcess checks if the given PID belongs to an sshd process.
//
// Returns true if it's an sshd process, false otherwise.
func isSSHDProcess(pid int) bool {
	// Read the process command line
	cmdlinePath := fmt.Sprintf("%s/%d/cmdline", getProcDir(), pid)
	cmdline, err := os.ReadFile(cmdlinePath)
	if err != nil {
		return false
	}

	// Check if the command line contains "sshd"
	return strings.Contains(string(cmdline), "sshd")
}
