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

use daemonize::{Daemonize, Outcome};
use getoptsargs::prelude::*;
use log::info;
use std::fs::{self, File};
use std::path::PathBuf;
use std::time::Duration;
use std::{env, io};
use xdg::BaseDirectories;

/// Maximum amount of time to wait for the child process to start when daemonization is enabled.
const MAX_CHILD_WAIT: Duration = Duration::from_secs(10);

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
        return Ok(s.split(":").map(PathBuf::from).collect());
    }

    default_agents_dirs()
}

/// Returns the default value of the `--log-file` flag.
fn default_log_file(xdg_dirs: &BaseDirectories) -> Result<PathBuf> {
    xdg_dirs
        .place_state_file("ssh-agent-switcher.log")
        .map_err(|e| anyhow!("Cannot create XDG_STATE_HOME: {}", e))
}

/// Gets the value of the `--log-file` flag, computing a default if necessary.
fn get_log_file(matches: &Matches, xdg_dirs: &BaseDirectories) -> Result<PathBuf> {
    match matches.opt_str("log-file") {
        Some(s) => Ok(PathBuf::from(s)),
        None => default_log_file(xdg_dirs),
    }
}

/// Returns the default value of the `--pid-file` flag.
fn default_pid_file(xdg_dirs: &BaseDirectories) -> Result<PathBuf> {
    match xdg_dirs.place_runtime_file("ssh-agent-switcher.pid") {
        Ok(path) => Ok(path),
        Err(_) => {
            // XDG_RUNTIME_DIR *must* be set, but it's quite annoying to fail when it's not.
            // The variable being missing is the default case for FreeBSD, so make this more
            // friendly in that case.
            xdg_dirs
                .place_state_file("ssh-agent-switcher.pid")
                .map_err(|e| anyhow!("Cannot create XDG_RUNTIME_DIR: {}", e))
        }
    }
}

/// Gets the value of the `--pid-file` flag, computing a default if necessary.
fn get_pid_file(matches: &Matches, xdg_dirs: &BaseDirectories) -> Result<PathBuf> {
    match matches.opt_str("pid-file") {
        Some(s) => Ok(PathBuf::from(s)),
        None => default_pid_file(xdg_dirs),
    }
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

    let xdg_dirs = BaseDirectories::new();
    if let Ok(log_file) = default_log_file(&xdg_dirs) {
        writeln!(output, "If --log-file is not set, the default path is {}", log_file.display())?;
    }
    if let Ok(pid_file) = default_pid_file(&xdg_dirs) {
        writeln!(output, "If --pid-file is not set, the default path is {}", pid_file.display())?;
    }

    Ok(())
}

fn app_setup(builder: Builder) -> Builder {
    builder
        .bugs("https://github.com/jmmv/ssh-agent-switcher/issues/")
        .copyright("Copyright 2023-2026 Julio Merino")
        .homepage("https://github.com/jmmv/ssh-agent-switcher/")
        .extra_help(app_extra_help)
        .disable_init_env_logger()
        .optopt(
            "",
            "agents-dirs",
            "colon-separated list of directories where to look for running agents",
            "dir1:..:dirn",
        )
        .optflag("", "daemon", "run in the background")
        .optopt("", "log-file", "path to the file where to write logs", "path")
        .optopt("", "pid-file", "path to the PID file to create", "path")
        .optopt("", "socket-path", "path to the socket to listen on", "path")
}

fn daemon_parent(socket_path: PathBuf, log_file: PathBuf, pid_file: PathBuf) -> Result<i32> {
    info!("Log file: {}", log_file.display());
    info!("PID file: {}", pid_file.display());
    let pid_content =
        ssh_agent_switcher::wait_for_file(&pid_file, MAX_CHILD_WAIT, fs::read_to_string)
            .map_err(|e| anyhow!("Daemon failed to start on time: {}", e))?;
    info!("PID is: {}", pid_content.trim());
    let _ = ssh_agent_switcher::wait_for_file(&socket_path, MAX_CHILD_WAIT, fs::metadata)
        .map_err(|e| anyhow!("Daemon failed to start on time: {}", e))?;
    Ok(0)
}

fn daemon_child(socket_path: PathBuf, agents_dirs: &[PathBuf], pid_file: PathBuf) -> Result<i32> {
    if let Err(e) = ssh_agent_switcher::run(socket_path, agents_dirs, pid_file) {
        bail!("{}", e);
    }
    Ok(0)
}

fn app_main(matches: Matches) -> Result<i32> {
    let xdg_dirs = BaseDirectories::new();

    let agents_dirs = get_agents_dirs(&matches)?;
    let log_file = get_log_file(&matches, &xdg_dirs)?;
    let pid_file = get_pid_file(&matches, &xdg_dirs)?;
    let socket_path = get_socket_path(&matches)?;

    if matches.opt_present("daemon") {
        let log =
            File::options().append(true).create(true).open(&log_file).map_err(|e| {
                anyhow!("Failed to open/create log file {}: {}", log_file.display(), e)
            })?;

        match Daemonize::new().pid_file(&pid_file).stderr(log).execute() {
            Outcome::Parent(Ok(_parent)) => {
                init_env_logger(&matches.program_name);
                daemon_parent(socket_path, log_file, pid_file)
            }
            Outcome::Parent(Err(e)) => {
                bail!("Failed to become daemon: {}", e);
            }
            Outcome::Child(Ok(_child)) => {
                init_env_logger(&matches.program_name);
                daemon_child(socket_path, &agents_dirs, pid_file)
            }
            Outcome::Child(Err(e)) => {
                let msg = e.to_string();
                if !msg.contains("unable to lock pid file") {
                    bail!("Failed to become daemon: {}", e);
                }
                Ok(0) // Already running.
            }
        }
    } else {
        init_env_logger(&matches.program_name);
        info!("Running in the foreground: ignoring --log-file and --pid-file");
        daemon_child(socket_path, &agents_dirs, pid_file)
    }
}

app!("ssh-agent-switcher", app_setup, app_main);
