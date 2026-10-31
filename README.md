# ssh-agent-switcher

[**SSH agent forwarding and tmux done right**](https://jmmv.dev/2023/11/ssh-agent-forwarding-and-tmux-done.html)

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
a responsive socket for each client connection and proxies that connection to
the real forwarded agent.

The daemon probes the liveliness of candidate sockets in parallel and uses the
first responsive connection while closing the others.  This is to avoid sockets
whose SSH connection has stalled (e.g. due to a laptop going to sleep).

## Installation

ssh-agent-switcher is written in Rust so you will need a standard Rust toolchain
in place.  See [rustup.rs](https://rustup.rs/) for installation instructions.

Then use `make` to build and install the binary in release mode along with its
manual page and supporting documentation:

```sh
make install MODE=release PREFIX="${HOME}/.local"
```

You may also use Cargo to install this program under `${HOME}/.cargo/bin`, but
the recommended method is to use `make install` as shown above because Cargo
will not install anything other than the program binary:

```sh
cargo install ssh-agent-switcher
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

Before sending any data, ssh-agent-switcher checks that the connected agent peer
belongs to your user account.  The daemon rejects connections when peer
credentials cannot be obtained, and keeps the authenticated connection for the
lifetime of each client.

*Do not run this as root.*
