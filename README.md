# ssh-agent-switcher

ssh-agent-switcher is a daemon that proxies SSH agent connections to any valid
forwarded agent provided by sshd.  This allows long-lived processes such as
terminal multiplexers like `tmux` or `screen` to access the connection-specific
forwarded agents.

More specifically, ssh-agent-switcher can be used to fix the problem that arises
in the following sequence of events:

1.  Connect to an SSH server with SSH agent forwarding.
1.  Start a tmux session in the SSH server.
1.  Detach the tmux session.
1.  Log out of the SSH server.
1.  Reconnect to the SSH server with SSH agent forwarding.
1.  Attach to the existing tmux session.
1.  Run an SSH command.
1.  See the command fail to communicate with the forwarded agent.

The ssh-agent-switcher daemon solves this problem by exposing an SSH agent
socket at a well-known location, allowing you to set `SSH_AUTH_SOCK` to a path
that does *not* change across different connections.  The daemon then looks for
a valid socket every time it receives a request and forwards the request to the
real forwarded agent.

## Installation

ssh-agent-switcher is written in Rust so you will need a standard Rust toolchain
in place.  See [rustup.rs](https://rustup.rs/) for installation instructions.

Then use `make` to build and install the binary in release mode:

```sh
make install MODE=release PREFIX="${HOME}/.local"
```

## Usage

Extend your login script (typically `~/.login`, `~/.bash_login`, or `~/.zlogin`)
with the following snippet:

```sh
~/.local/bin/ssh-agent-switcher --daemon 2>/dev/null || true
export SSH_AUTH_SOCK="/tmp/ssh-agent.${USER}"
```

For `fish`, extend `~/.config/fish/config.fish` with the following:

```sh
~/.local/bin/ssh-agent-switcher --daemon &>/dev/null || true
set -gx SSH_AUTH_SOCK "/tmp/ssh-agent.$USER"
```

## Security considerations

ssh-agent-switcher is intended to run under your personal unprivileged account
and does not cross any security boundaries.  All this daemon does is expose a
new socket that only you can access and forwards all communication to another
socket to which you must already have access.

*Do not run this as root.*
