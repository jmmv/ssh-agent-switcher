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
a valid socket every time it receives a request and forwards the request to the
real forwarded agent.

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

<details><summary><h2>Nix usage</h2></summary>

Reference ssh-agent-switcher as a flake input and pass it to your home-manager modules:
```nix
{
    inputs = {
        # Rest of your config...
        ssh-agent-switcher = {
          url = "github:jmmv/ssh-agent-switcher";
        };
    };
    outputs = {
        # Rest of your inputs...
        ssh-agent-switcher,
        ...
    } : {
      nixosConfigurations = {
        someConfig = nixpkgs.lib.nixosSystem {
          modules = [
            # ...
            {
              home-manager.extraSpecialArgs = { inherit ssh-agent-switcher; };
              home-manager.users.some-user = import ./home.nix;
            }
          ];
        };
      };
    };
}
```

Extend your login script within your home-manager module:
You only need to set the config for the shell you use. Don't forget to change `x86_64-linux` if you're on a different system.

```nix
{ ssh-agent-switcher, ... } : {
  # ...
  programs.zsh.loginExtra = ''
    ${ssh-agent-switcher.packages.x86_64-linux.default}/bin/ssh-agent-switcher --daemon 2>/dev/null || true
    export SSH_AUTH_SOCK="/tmp/ssh-agent.''${USER}"
  '';

  programs.bash.profileExtra = ''
    ${ssh-agent-switcher.packages.x86_64-linux.default}/bin/ssh-agent-switcher --daemon 2>/dev/null || true
    export SSH_AUTH_SOCK="/tmp/ssh-agent.''${USER}"
  '';

  programs.fish.loginShellInit = ''
    ${ssh-agent-switcher.packages.x86_64-linux.default}/bin/ssh-agent-switcher --daemon &>/dev/null || true
    set -gx SSH_AUTH_SOCK "/tmp/ssh-agent.''$USER"
  '';
}
```

</details>

## Security considerations

ssh-agent-switcher is intended to run under your personal unprivileged account
and does not cross any security boundaries.  All this daemon does is expose a
new socket that only you can access and forwards all communication to another
socket to which you must already have access.

*Do not run this as root.*
