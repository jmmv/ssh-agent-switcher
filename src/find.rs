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

//! Discovery of candidate SSH agent socket paths.

use log::{debug, trace};
use std::collections::BTreeSet;
use std::fs;
use std::io::{ErrorKind, Result};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

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

/// Rejects paths that cannot be sshd agent sockets before connecting.
fn check_socket(path: &Path) -> Result<()> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err(error!(ErrorKind::InvalidInput, "Invalid socket path"));
    };
    if !name.starts_with("agent.") && !name.contains(".sshd.") {
        return Err(error!(
            ErrorKind::InvalidInput,
            "Socket name does not start with 'agent.' or contain '.sshd.'"
        ));
    }
    let metadata = fs::metadata(path)?;
    if !metadata.file_type().is_socket() {
        return Err(error!(ErrorKind::InvalidInput, "Path is not a socket"));
    }
    Ok(())
}

/// Adds candidate agent socket paths found directly in `dir`.
fn find_in_subdir(dir: &Path, candidates: &mut BTreeSet<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            debug!("Failed to read directory entries in {}: {}", dir.display(), e);
            return;
        }
    };
    let mut found_socket = false;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                debug!("Failed to read directory entry in {}: {}", dir.display(), e);
                continue;
            }
        };
        let path = entry.path();
        match check_socket(&path) {
            Ok(()) => {
                found_socket = true;
                candidates.insert(path);
            }

            Err(e) => trace!("Ignoring candidate socket {}: {}", path.display(), e),
        }
    }
    if !found_socket {
        debug!("Ignoring {}: No socket in subdirectory", dir.display());
    }
}

/// Checks that a shared-directory entry has an sshd session name and belongs to `uid`.
fn check_shared_subdir(dir: &Path, uid: libc::uid_t) -> Result<()> {
    let Some(name) = dir.file_name().and_then(|name| name.to_str()) else {
        return Err(error!(ErrorKind::InvalidInput, "Invalid file name"));
    };
    if !name.starts_with("ssh-") {
        return Err(error!(ErrorKind::InvalidInput, "Basename does not start with 'ssh-'"));
    }
    let metadata = fs::metadata(dir)?;
    if metadata.uid() != uid {
        return Err(error!(
            ErrorKind::InvalidInput,
            "{} is owned by {}, not the current user {}",
            dir.display(),
            metadata.uid(),
            uid
        ));
    }
    Ok(())
}

/// Collects candidate sockets from sshd session directories owned by `uid`.
fn find_in_shared_dir(dir: &Path, uid: libc::uid_t, candidates: &mut BTreeSet<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            debug!("Failed to read directory entries in {}: {}", dir.display(), e);
            return;
        }
    };
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
            Ok(_) => {
                trace!("Ignoring {}: not a directory", path.display());
                continue;
            }

            Err(e) => {
                trace!("Ignoring {}: {}", path.display(), e);
                continue;
            }
        }
        match check_shared_subdir(&path, uid) {
            Ok(()) => find_in_subdir(&path, candidates),
            Err(e) => trace!("Ignoring {}: {}", path.display(), e),
        }
    }
}

/// Collects candidate paths using sshd naming and ownership checks.
pub(super) fn find_sockets(
    dirs: &[PathBuf],
    home: Option<&Path>,
    uid: libc::uid_t,
) -> BTreeSet<PathBuf> {
    let mut candidates = BTreeSet::new();
    for dir in dirs {
        if let Some(home) = home
            && dir.starts_with(home)
        {
            find_in_subdir(dir, &mut candidates);
        }
        find_in_shared_dir(dir, uid, &mut candidates);
    }
    candidates
}
