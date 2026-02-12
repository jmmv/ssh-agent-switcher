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
use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
use signal_hook::iterator::Signals;
use std::env;
use std::fs;
use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

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
pub fn create_listener(socket_path: &Path) -> Result<UnixListener> {
    // Ensure the socket is not group nor world readable so that we don't expose the real socket
    // indirectly to other users.
    let _guard = set_umask(0o177);

    UnixListener::bind(socket_path)
        .map_err(|e| format!("Cannot listen on {}: {}", socket_path.display(), e))
}

/// Copies data bidirectionally between two streams until one side closes.
fn handle_bi_socket_forwarding(client: UnixStream, agent: UnixStream) -> io::Result<()> {
    let mut client_read = client;
    let mut agent_read = agent;
    let mut client_write = client_read.try_clone()?;
    let mut agent_write = agent_read.try_clone()?;

    let t1 = thread::spawn(move || io::copy(&mut client_read, &mut agent_write));
    let t2 = thread::spawn(move || io::copy(&mut agent_read, &mut client_write));

    // Wait for either direction to finish (one side closed)
    let r1 = t1.join().map_err(|_| io::Error::other("thread panicked"))?;
    let r2 = t2.join().map_err(|_| io::Error::other("thread panicked"))?;

    r1.and(r2).map(|_| ())
}

/// Handles one incoming connection on `client`.
fn handle_connection(
    client: UnixStream,
    agents_dirs: Arc<[PathBuf]>,
    home: Option<PathBuf>,
    uid: libc::uid_t,
) {
    let agent = match find::find_socket(&agents_dirs, home.as_deref(), uid) {
        Some(socket) => socket,
        None => {
            warn!("Dropping connection: no agent found");
            return;
        }
    };
    if let Err(e) = handle_bi_socket_forwarding(client, agent) {
        warn!("Connection error: {}", e);
    }
    debug!("Closing client connection");
}

/// Runs the core logic of the app.
///
/// This serves the SSH agent socket using the provided `listener` and looks for sshd sockets
/// in `agents_dirs`.
///
/// The `pid_file` is needed for cleanup purposes. If `systemd_activated` is true, the socket
/// file will not be removed on exit (systemd owns it).
pub fn run(
    listener: UnixListener,
    agents_dirs: &[PathBuf],
    pid_file: PathBuf,
    systemd_activated: bool,
) -> Result<()> {
    let socket_path = listener
        .local_addr()
        .ok()
        .and_then(|addr| addr.as_pathname().map(|p| p.to_path_buf()))
        .ok_or_else(|| "Cannot determine socket path from listener".to_string())?;

    let home = env::var("HOME").map(|v| Some(PathBuf::from(v))).unwrap_or(None);
    let uid = unsafe { libc::getuid() };
    let agents_dirs: Arc<[PathBuf]> = agents_dirs.into();

    // Set up signal handling with the atomic flag + reconnect pattern
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = Arc::clone(&shutdown);
    let socket_path_clone = socket_path.clone();

    let mut signals = Signals::new([SIGHUP, SIGINT, SIGQUIT, SIGTERM])
        .map_err(|e| format!("Failed to install signal handlers: {}", e))?;

    thread::spawn(move || {
        for sig in signals.forever() {
            match sig {
                SIGHUP => {
                    // Reload - currently a no-op
                }
                SIGINT | SIGQUIT | SIGTERM => {
                    shutdown_clone.store(true, Ordering::SeqCst);
                    // Connect to our own socket to wake up the accept() call
                    let _ = UnixStream::connect(&socket_path_clone);
                    break;
                }
                _ => {}
            }
        }
    });

    debug!("Entering main loop");
    loop {
        match listener.accept() {
            Ok((socket, _addr)) => {
                if shutdown.load(Ordering::SeqCst) {
                    if systemd_activated {
                        info!("Shutting down (systemd owns {})", socket_path.display());
                    } else {
                        info!("Shutting down and removing {}", socket_path.display());
                    }
                    break;
                }

                debug!("Connection accepted");
                let agents_dirs = Arc::clone(&agents_dirs);
                let home = home.clone();
                thread::spawn(move || handle_connection(socket, agents_dirs, home, uid));
            }
            Err(e) => {
                if shutdown.load(Ordering::SeqCst) {
                    if systemd_activated {
                        info!("Shutting down (systemd owns {})", socket_path.display());
                    } else {
                        info!("Shutting down and removing {}", socket_path.display());
                    }
                    break;
                }
                warn!("Failed to accept connection: {}", e);
            }
        }
    }
    debug!("Main loop exited");

    // Don't remove socket if systemd owns it
    if !systemd_activated {
        let _ = fs::remove_file(&socket_path);
    }
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
