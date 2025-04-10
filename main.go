// Copyright 2023 Julio Merino.
// All rights reserved.
//
// Redistribution and use in source and binary forms, with or without modification, are permitted
// provided that the following conditions are met:
//
// * Redistributions of source code must retain the above copyright notice, this list of conditions
//   and the following disclaimer.
// * Redistributions in binary form must reproduce the above copyright notice, this list of
//   conditions and the following disclaimer in the documentation and/or other materials provided with
//   the distribution.
// * Neither the name of rules_shtk nor the names of its contributors may be used to endorse or
//   promote products derived from this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR
// IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND
// FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR
// CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
// DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
// DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
// WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY
// WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

// ssh-agent-switcher serves a Unix domain socket that proxies connections to any valid SSH agent
// socket provided by sshd.
package main

import (
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"sort"
	"strings"
	"syscall"
)

var (
	socketPath = flag.String("socketPath", defaultSocketPath(), "path to the socket to listen on")
	agentsDir  = flag.String("agentsDir", "/tmp", "directory where to look for running agents")
)

// defaultSocketPath computes the name of the default value for the socketPath flag.
func defaultSocketPath() string {
	user := os.Getenv("USER")
	if user == "" {
		return ""
	}
	return fmt.Sprintf("/tmp/ssh-agent.%s", user)
}

// findAgentSocketSubdir scans the contents of "dir", which should point to a session directory
// createdy by sshd, looks for a valid "agent.*" socket, opens it, and returns the connection to
// the agent.
//
// This tries all possible files in search for a socket and only returns an error if no valid
// and alive candidate can be found.
func findAgentSocketSubdir(dir string) (net.Conn, error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}

	for _, entry := range entries {
		path := filepath.Join(dir, entry.Name())

		if !strings.HasPrefix(entry.Name(), "agent.") {
			log.Printf("Ignoring %s: does not start with 'agent.'\n", path)
			continue
		}

		fi, err := os.Stat(path)
		if err != nil {
			log.Printf("Ignoring %s: stat failed: %v\n", path, err)
			continue
		}

		if fi.Mode()&os.ModeSocket == 0 {
			log.Printf("Ignoring %s: not a socket\n", path)
			continue
		}

		// Check if the owning process is an sshd with a PTY attached
		pid, err := getSocketOwnerPid(path)
		if err != nil {
			log.Printf("Ignoring %s: %v\n", path, err)
			continue
		}

		if !isSSHDProcess(pid) {
			log.Printf("Ignoring %s: not owned by sshd process\n", path)
			continue
		}

		if !hasAttachedPts(pid) {
			log.Printf("Ignoring %s: owning sshd process does not have a PTS attached\n", path)
			continue
		}

		conn, err := net.Dial("unix", path)
		if err != nil {
			log.Printf("Ignoring %s: open failed: %v\n", path, err)
			continue
		}

		log.Printf("Successfully opened SSH agent at %s", path)
		return conn, nil
	}

	return nil, errors.New("no socket in directory")
}

// findAgentSocket scans the contents of "dir", which should point to the directory where
// sshd places the session directories for forwarded agents, looks for a valid connection to
// an agent, opens the agent's socket, and returns the connection to the agent.
//
// This tries all possible directories in search for a socket and only returns an error if
// no valid and alive candidate can be found.
func findAgentSocket(dir string) (net.Conn, error) {
	// It is tempting to use the *at family of system calls to avoid races when checking for
	// file metadata before opening the socket... but there is no guarantee that the sshd
	// instance will be present at all even after we open the socket, so the races don't
	// matter.  Also note that these checks are not meant to protect us against anything in
	// terms of security: they are merely to keep things speedy and nice.

	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}

	// The sorting is unnecessary but it helps with testing certain conditions.
	sort.Slice(entries, func(i, j int) bool {
		return entries[i].Name() < entries[j].Name()
	})

	ourUid := os.Getuid()
	for _, entry := range entries {
		path := filepath.Join(dir, entry.Name())

		if !entry.IsDir() {
			log.Printf("Ignoring %s: not a directory\n", path)
			continue
		}

		if !strings.HasPrefix(entry.Name(), "ssh-") {
			log.Printf("Ignoring %s: does not start with 'ssh-'\n", path)
			continue
		}

		fi, err := os.Stat(path)
		if err != nil {
			log.Printf("Ignoring %s: stat failed: %v\n", path, err)
			continue
		}

		// This check is not strictly necessary: if we found sshd sockets owned by other users, we
		// would simply fail to open them later anyway.
		uid := fi.Sys().(*syscall.Stat_t).Uid
		if int(uid) != ourUid {
			log.Printf("Ignoring %s: owner %d is not current user %d\n", path, uid, ourUid)
			continue
		}

		agent, err := findAgentSocketSubdir(path)
		if err != nil {
			log.Printf("Ignoring %s: %v\n", path, err)
			continue
		}
		return agent, nil
	}

	return nil, errors.New("agent not found")
}

// proxyConnection forwards all request from the client to the agent, and all responses from
// the agent to the client.
func proxyConnection(client net.Conn, agent net.Conn) error {
	// The buffer needs to be large enough to handle any one read or write by the client or
	// the agent.  Otherwise bad things will happen.
	//
	// TODO(jmerino): This could be improved but it's better to keep it simple.  In particular,
	// fixing this properly would require either spawning extra coroutines which, while they are
	// cheap, they are tricky to handle; or it would require a way to perform non-blocking reads
	// from the socket, which is not supported yet: https://github.com/golang/go/issues/15735.
	buf := make([]byte, 4096)

	for {
		n, err := client.Read(buf)
		if err != nil {
			if err != io.EOF {
				return fmt.Errorf("read from client failed: %v", err)
			}
			break
		}
		if n == 0 {
			break
		}

		_, err = agent.Write(buf[:n])
		if err != nil {
			return fmt.Errorf("write to agent failed: %v", err)
		}

		n, err = agent.Read(buf)
		if err != nil {
			return fmt.Errorf("read from agent failed: %v", err)
		}

		if n > 0 {
			_, err = client.Write(buf[:n])
			if err != nil {
				return fmt.Errorf("write to client failed: %v", err)
			}
		}
	}

	return nil
}

// handleConnection receives a connection from the client, looks for an sshd serving an agent,
// and proxies the connection to it.
func handleConnection(client net.Conn) {
	log.Printf("Accepted client connection")
	defer client.Close()

	agent, err := findAgentSocket(*agentsDir)
	if err != nil {
		log.Printf("Dropping connection: %v", err)
		return
	}
	defer agent.Close()

	if err := proxyConnection(client, agent); err != nil {
		log.Printf("Dropping connection: %v", err)
		return
	}
	log.Printf("Closing client connection")
}

// setupSignals installs signal handlers to clean up files and ignores signals that we don't want
// to cause us to exit.
func setupSignals(socketPath string) {
	// Prevent terminal disconnects from killing this process if started in the background.
	signal.Ignore(syscall.SIGHUP)

	// Clean up the socket we create on exit.
	c := make(chan os.Signal, 1)
	signal.Notify(c, os.Interrupt, syscall.SIGTERM)
	go func() {
		<-c
		log.Printf("Shutting down due to signal and deleting %s\n", socketPath)
		os.Remove(socketPath)
		os.Exit(1)
	}()
}

func main() {
	flag.Parse()
	if len(flag.Args()) != 0 {
		log.Fatal("No arguments allowed")
	}

	// Install signal handlers before we create the socket so that we don't leave it
	// behind in any case.
	setupSignals(*socketPath)

	// Ensure the socket is not group nor world readable so that we don't expose the
	// real socket indirectly to other users.
	syscall.Umask(0177)
	socket, err := net.Listen("unix", *socketPath)
	if err != nil {
		log.Fatal(err)
	}
	log.Printf("Listening on %s", *socketPath)

	for {
		conn, err := socket.Accept()
		if err != nil {
			log.Fatal(err)
		}

		go handleConnection(conn)
	}
}
