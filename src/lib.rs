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
use signal_hook::{consts::SIGHUP, consts::TERM_SIGNALS, iterator::Signals};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::{env, fs, io};

mod find;
mod proxy;

/// Error type for this crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Error while trying to find the proxied SSH agent socket.
    #[error("{0}")]
    FindError(String),

    /// Error while proxying the SSH agent request.
    #[error("{0}")]
    ProxyError(String),

    /// Error during program setup or teardown.
    #[error("{0}")]
    SetupError(String),
}

/// Result type for this crate.
type Result<T> = std::result::Result<T, Error>;

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

/// Installs global signal handlers for termination signals.
///
/// Returns a thread that blocks until any of the signals is received and immediately deletes
/// `socket_path` before returning.
fn setup_signals(socket_path: &Path, stop: Arc<AtomicBool>) -> Result<JoinHandle<()>> {
    let mut sigs = vec![SIGHUP];
    sigs.extend(TERM_SIGNALS);
    let mut signals = Signals::new(&sigs).map_err(|e| {
        Error::SetupError(format!("Cannot set up termination signal handlers: {}", e))
    })?;

    let handle = {
        let socket_path = socket_path.to_owned();
        thread::spawn(move || {
            for sig in signals.forever() {
                if TERM_SIGNALS.contains(&sig) {
                    info!(
                        "Shutting down due to signal {:?} and deleting {}",
                        sig,
                        socket_path.display()
                    );
                    break;
                }
                debug!("Ignoring signal {:?}", sig);
            }

            let _ = fs::remove_file(socket_path);
            stop.store(true, Ordering::Relaxed);
        })
    };

    Ok(handle)
}

/// Creates the agent socket to listen on.
///
/// This makes sure that the socket is only accessible by the current user.
fn create_listener(socket_path: &Path) -> Result<UnixListener> {
    // Ensure the socket is not group nor world readable so that we don't expose the real socket
    // indirectly to other users.
    let _guard = set_umask(0o177);

    UnixListener::bind(socket_path).map_err(|e| {
        Error::SetupError(format!("Cannot listen on {}: {}", socket_path.display(), e))
    })
}

/// Handles one incoming connection on `client`.
fn handle_connection(
    mut client: UnixStream,
    agents_dirs: &[PathBuf],
    home: Option<&Path>,
    uid: libc::uid_t,
) -> Result<()> {
    let mut agent = match find::find_socket(agents_dirs, home, uid) {
        Some(socket) => socket,
        None => {
            return Err(Error::FindError("No agent found; cannot proxy request".to_owned()));
        }
    };
    let result = proxy::proxy_request(&mut client, &mut agent)
        .map_err(|e| Error::ProxyError(format!("{}", e)));
    debug!("Closing client connection");
    result
}

/// Runs the core logic of the app.
pub fn run(socket_path: PathBuf, agents_dirs: &[PathBuf]) -> Result<()> {
    let home = env::var("HOME").map(|v| Some(PathBuf::from(v))).unwrap_or(None);
    let uid = unsafe { libc::getuid() };

    // Install signal handlers before we create the socket so that we don't leave it behind in any
    // case.
    let stop = Arc::from(AtomicBool::new(false));
    let handle = setup_signals(&socket_path, stop.clone())?;

    let listener = create_listener(&socket_path)?;

    // TODO(jmmv): signal_hook forcibly enables `SA_RESTART` so, for simplicity, we do active
    // polling of the termination condition.  This is ugly though: we should use a pipe and select
    // below.
    listener
        .set_nonblocking(true)
        .map_err(|e| Error::SetupError(format!("Cannot set socket to non-blocking: {}", e)))?;

    debug!("Entering main loop");
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((socket, _addr)) => {
                debug!("Connection accepted");
                // TODO(jmmv): Connections are handled sequentially.  This is just fine for this
                // program, but if we had an easier way to do asynchronous operations, we could
                // fix this.
                if let Err(e) = handle_connection(socket, agents_dirs, home.as_deref(), uid) {
                    warn!("Dropping connection due to error: {}", e);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => warn!("Failed to accept connection: {}", e),
        };
    }
    debug!("Main loop exited");

    handle.join().map_err(|_| Error::SetupError(format!("Failed to wait for signals")))
}
