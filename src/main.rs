// Copyright 2025 Julio Merino.
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
// * Neither the name of ssh-agent-switcher nor the names of its contributors may be used to endorse
//   or promote products derived from this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR
// IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND
// FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR
// CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
// DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
// DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
// WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY
// WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

//! Serves a Unix domain socket that proxies connections to any valid SSH agent provided by sshd.

use getoptsargs::prelude::*;
use std::path::PathBuf;
use std::{env, io};

/// Checks if the required `name` variable is present and returns its value.
fn get_required_env_var(name: &str) -> Result<String> {
    match env::var(name) {
        Ok(value) => Ok(value),
        Err(e) => bail!("{} variable is set but is not valid: {}", name, e),
    }
}

/// Returns the default value of the `--agents-dirs` flag.
fn default_agents_dirs() -> Result<Vec<PathBuf>> {
    // OpenSSH 10.1 moved agent sockets from /tmp to the user's home directory and uses
    // a different naming scheme (no subdirectories and different names).
    let home = get_required_env_var("HOME")?;
    Ok(vec![PathBuf::from(format!("{}/.ssh/agent", home)), PathBuf::from("/tmp")])
}

/// Gets the value of the `--agents-dirs` flag, computing a default if necessary.
fn get_agents_dirs(matches: &Matches) -> Result<Vec<PathBuf>> {
    if let Some(s) = matches.opt_str("agents-dirs") {
        return Ok(s.split(":").into_iter().map(PathBuf::from).collect());
    }

    default_agents_dirs()
}

/// Returns the default value of the `--socket-path` flag.
fn default_socket_path() -> Result<PathBuf> {
    let user = get_required_env_var("USER")?;
    Ok(PathBuf::from(format!("/tmp/ssh-agent.{}", user)))
}

/// Gets the value of the `--socket-path` flag, computing a default if necessary.
fn get_socket_path(matches: &Matches) -> Result<PathBuf> {
    if let Some(s) = matches.opt_str("socket-path") {
        return Ok(PathBuf::from(s));
    }

    default_socket_path()
}

fn app_extra_help(output: &mut dyn io::Write) -> io::Result<()> {
    if let Ok(agents_dirs) = default_agents_dirs() {
        writeln!(
            output,
            "If --agents-dirs is not set, the default lookup location is: {}",
            agents_dirs
                .into_iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<String>>()
                .join(":")
        )?;
    }
    if let Ok(socket_path) = default_socket_path() {
        writeln!(
            output,
            "If --socket-path is not set, the default path is: {}",
            socket_path.display()
        )?;
    }
    Ok(())
}

fn app_setup(builder: Builder) -> Builder {
    builder
        .bugs("https://github.com/jmmv/ssh-agent-switcher/issues/")
        .copyright("Copyright 2023-2025 Julio Merino")
        .homepage("https://github.com/jmmv/ssh-agent-switcher/")
        .extra_help(app_extra_help)
        .optopt(
            "",
            "agents-dirs",
            "colon-separated list of directories where to look for running agents",
            "dir1:..:dirn",
        )
        .optopt("", "socket-path", "path to the socket to listen on", "path")
}

fn app_main(matches: Matches) -> Result<i32> {
    let socket_path = get_socket_path(&matches)?;
    let agents_dirs = get_agents_dirs(&matches)?;

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    match ssh_agent_switcher::run(socket_path, &agents_dirs) {
        Ok(()) => Ok(0),
        Err(e) => {
            eprintln!("ERROR: {}", e);
            Ok(1)
        }
    }
}

app!("ssh-agent-switcher", app_setup, app_main);
