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

//! Utilities to find the correct SSH agent socket.

use log::{debug, info, trace};
use std::io::{ErrorKind, Result};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::{fs, path::PathBuf};

/// Syntactic sugar to instantiate an error.
#[macro_export]
macro_rules! error {
    ( $kind:expr, $text:expr ) => {
        std::io::Error::new($kind, $text)
    };

    ( $kind:expr, $fmt:literal $(, $args:expr)+ ) => {
        std::io::Error::new($kind, format!($fmt $(, $args)+))
    };
}

/// Attempts to open the socket `path`.
fn try_open(path: &Path) -> Result<UnixStream> {
    let name = path.file_name().expect(
        "The path comes from joining a directory to one of its entries, so it must have a name",
    );
    let name = match name.to_str() {
        Some(name) => name,
        None => return Err(error!(ErrorKind::InvalidInput, "Invalid socket path")),
    };

    let is_pre_openssh_10_1 = name.starts_with("agent.");
    let is_openssh_10_1 = name.contains(".sshd.");
    if !is_pre_openssh_10_1 && !is_openssh_10_1 {
        return Err(error!(
            ErrorKind::InvalidInput,
            "Socket name in does not start with 'agent.' or does not contain '.sshd.'"
        ));
    }

    let metadata =
        fs::metadata(&path).map_err(|e| error!(e.kind(), "Failed to get metadata: {}", e))?;

    if (metadata.mode() & libc::S_IFSOCK as u32) == 0 {
        return Err(error!(ErrorKind::InvalidInput, "Path is not a socket"));
    }

    let socket = UnixStream::connect(&path)
        .map_err(|e| error!(e.kind(), "Cannot connect to socket: {}", e))?;

    Ok(socket)
}

/// Scans the contents of `dir`, which should point to a session directory created by sshd, looks
/// for a valid socket, opens it, and returns the connection to the agent.
///
/// This tries all possible files in search for a socket and only returns an error if no valid
/// and alive candidate can be found.
fn find_in_subdir(dir: &Path) -> Option<UnixStream> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            debug!("Failed to read directory entries in {}: {}", dir.display(), e);
            return None;
        }
    };

    let mut candidates = vec![];
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                debug!("Failed to read directory entry in {}: {}", dir.display(), e);
                continue;
            }
        };

        let candidate = entry.path();
        candidates.push(candidate);
    }

    // The sorting is unnecessary but it helps with testing certain conditions.
    candidates.sort();

    for candidate in candidates {
        let socket = match try_open(&candidate) {
            Ok(socket) => socket,
            Err(e) => {
                trace!("Ignoring candidate socket {}: {}", candidate.display(), e);
                continue;
            }
        };

        info!("Successfully opened socket at {}", candidate.display());
        return Some(socket);
    }

    debug!("No socket in directory {}", dir.display());
    None
}

/// Scans the contents of `dir`, which should point to one of the directories where sshd places the
/// session directories for forwarded agents, looks for a valid connection to an agent, opens the
/// agent's socket, and returns the connection to the agent.
fn try_shared_subdir(dir: &Path, uid: libc::uid_t) -> Result<UnixStream> {
    // It is tempting to use the *at family of system calls to avoid races when checking for
    // file metadata before opening the socket... but there is no guarantee that the sshd
    // instance will be present at all even after we open the socket, so the races don't
    // matter.  Also note that these checks are not meant to protect us against anything in
    // terms of security: they are merely to keep things speedy and nice.

    let name = dir.file_name().expect(
            "The candidate path comes from joining a directory to one of its entries, so it must have a name");
    let name = match name.to_str() {
        Some(name) => name,
        None => return Err(error!(ErrorKind::InvalidInput, "Invalid file name")),
    };

    if !name.starts_with("ssh-") {
        return Err(error!(ErrorKind::InvalidInput, "Basename does not start with 'ssh-'"));
    }

    let metadata = fs::metadata(&dir).map_err(|e| error!(e.kind(), "Stat failed: {}", e))?;

    if metadata.uid() != uid {
        return Err(error!(
            ErrorKind::InvalidInput,
            "{} is owned by {}, not the current user {}",
            dir.display(),
            metadata.uid(),
            uid
        ));
    }

    match find_in_subdir(dir) {
        Some(socket) => Ok(socket),
        None => return Err(error!(ErrorKind::NotFound, "No socket in subdirectory")),
    }
}

/// Scans the contents of `dir`, which should point to the directory where sshd places the session
/// directories for forwarded agents, looks for a valid connection to an agent, opens the agent's
/// socket, and returns the connection to the agent.
///
/// This tries all possible directories in search for a socket and only returns an error if no valid
/// and alive candidate can be found.
fn find_in_shared_dir(dir: &Path, our_uid: libc::uid_t) -> Option<UnixStream> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            debug!("Failed to read directory entries in {}: {}", dir.display(), e);
            return None;
        }
    };

    let mut subdirs = vec![];
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                debug!("Failed to read directory entry in {}: {}", dir.display(), e);
                continue;
            }
        };
        let path = entry.path();

        match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => (),
            Ok(_file_type) => {
                trace!("Ignoring {}: not a directory", path.display());
                continue;
            }
            Err(e) => {
                trace!("Ignoring {}: {}", path.display(), e);
                continue;
            }
        };

        subdirs.push(path);
    }

    // The sorting is unnecessary but it helps with testing certain conditions.
    subdirs.sort();

    for subdir in subdirs {
        let socket = match try_shared_subdir(&subdir, our_uid) {
            Ok(socket) => socket,
            Err(e) => {
                trace!("Ignoring {}: {}", subdir.display(), e);
                continue;
            }
        };

        return Some(socket);
    }

    debug!("No socket in directory: {}", dir.display());
    None
}

/// Scans the contents of `dirs`, which should point to one or more session directories created
/// by sshd, looks for a valid socket, opens it, and returns the connection to the agent.
///
/// This tries all possible files in search for a socket and only returns an error if no valid
/// and alive candidate can be found.
pub(super) fn find_socket(
    dirs: &[PathBuf],
    home: Option<&Path>,
    uid: libc::uid_t,
) -> Option<UnixStream> {
    for dir in dirs {
        if let Some(home) = home {
            if dir.starts_with(home) {
                debug!("Looking for an agent socket in {} with HOME naming scheme", dir.display());
                if let Some(socket) = find_in_subdir(dir) {
                    return Some(socket);
                }
            }
        }

        debug!("Looking for an agent socket in {} subdirs", dir.display());
        if let Some(socket) = find_in_shared_dir(dir, uid) {
            return Some(socket);
        }
    }

    None
}
