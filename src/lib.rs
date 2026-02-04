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

use log::{debug, info, warn};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio::select;
use tokio::signal::unix::{SignalKind, signal};

mod find;

/// Result type for this crate.
type Result<T> = std::result::Result<T, String>;

/// A scope guard to restore the previous umask.
struct UmaskGuard {
    old_umask: libc::mode_t,
}

impl Drop for UmaskGuard {
    fn drop(&mut self) {
        let _ = unsafe { libc::umask(self.old_umask) };
    }
}

/// Sets the umask and returns a guard to restore it on drop.
fn set_umask(umask: libc::mode_t) -> UmaskGuard {
    UmaskGuard { old_umask: unsafe { libc::umask(umask) } }
}

/// Creates the agent socket to listen on.
///
/// This makes sure that the socket is only accessible by the current user.
fn create_listener(socket_path: &Path) -> Result<UnixListener> {
    // Ensure the socket is not group nor world readable so that we don't expose the real socket
    // indirectly to other users.
    let _guard = set_umask(0o177);

    UnixListener::bind(socket_path)
        .map_err(|e| format!("Cannot listen on {}: {}", socket_path.display(), e))
}

/// Handles one incoming connection on `client`.
async fn handle_connection(
    mut client: UnixStream,
    agents_dirs: &[PathBuf],
    home: Option<&Path>,
    uid: libc::uid_t,
) -> Result<()> {
    let mut agent = match find::find_socket(agents_dirs, home, uid).await {
        Some(socket) => socket,
        None => {
            return Err("No agent found; cannot proxy request".to_owned());
        }
    };
    let result = tokio::io::copy_bidirectional(&mut client, &mut agent)
        .await.map(|_| ())
        .map_err(|e| format!("{}", e));
    debug!("Closing client connection");
    result
}

/// Runs the core logic of the app.
///
/// This serves the SSH agent socket on `socket_path` and looks for sshd sockets in `agents_dirs`.
///
/// The `pid_file` needs to be passed in for cleanup purposes.
pub async fn run(socket_path: PathBuf, agents_dirs: &[PathBuf], pid_file: PathBuf) -> Result<()> {
    let home = env::var("HOME").map(|v| Some(PathBuf::from(v))).unwrap_or(None);
    let uid = unsafe { libc::getuid() };

    let mut sighup = signal(SignalKind::hangup())
        .map_err(|e| format!("Failed to install SIGHUP handler: {}", e))?;
    let mut sigint = signal(SignalKind::interrupt())
        .map_err(|e| format!("Failed to install SIGINT handler: {}", e))?;
    let mut sigquit = signal(SignalKind::quit())
        .map_err(|e| format!("Failed to install SIGQUIT handler: {}", e))?;
    let mut sigterm = signal(SignalKind::terminate())
        .map_err(|e| format!("Failed to install SIGTERM handler: {}", e))?;

    let listener = create_listener(&socket_path)?;

    debug!("Entering main loop");
    let mut stop = None;
    while stop.is_none() {
        select! {
            result = listener.accept() => match result {
                Ok((socket, _addr)) => {
                    debug!("Connection accepted");
                    // TODO(jmmv): Connections are handled sequentially.  This is... fine.
                    if let Err(e) = handle_connection(socket, agents_dirs, home.as_deref(), uid).await {
                        warn!("Dropping connection due to error: {}", e);
                    }
                }
                Err(e) => warn!("Failed to accept connection: {}", e),
            },

            _ = sighup.recv() => (),
            _ = sigint.recv() => stop = Some("SIGINT"),
            _ = sigquit.recv() => stop = Some("SIGQUIT"),
            _ = sigterm.recv() => stop = Some("SIGTERM"),
        }
    }
    debug!("Main loop exited");

    let stop = stop.expect("Loop can only exit by setting stop");
    info!("Shutting down due to {} and removing {}", stop, socket_path.display());

    let _ = fs::remove_file(&socket_path);
    // Because we catch signals, daemonize doesn't properly clean up the PID file so we have
    // to do it ourselves.
    let _ = fs::remove_file(&pid_file);

    Ok(())
}

/// Waits for `path` to exist for a maximum period of time using operation `op`.
/// Returns the result of `op` on success.
pub fn wait_for_file<P: AsRef<Path> + Copy, T>(
    path: P,
    mut pending_wait: Duration,
    op: fn(P) -> io::Result<T>,
) -> Result<T> {
    while pending_wait > Duration::ZERO {
        match op(path) {
            Ok(result) => {
                return Ok(result);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                pending_wait -= Duration::from_millis(1);
            }
            Err(e) => {
                return Err(e.to_string());
            }
        }
    }
    Err("File was not created on time".to_owned())
}
