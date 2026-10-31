// Copyright 2026 Julio Merino.
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

//! Health probes and selection of responsive SSH agent connections.
//!
//! Messages start with a four-byte, big-endian payload length, then a one-byte type and its fields.
//! The length includes the type byte but excludes the length field itself.

use crate::error;
use log::{debug, info};
use std::collections::BTreeSet;
use std::io::{ErrorKind, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::task::JoinSet;
use tokio::time::timeout;

/// Client request for the agent's public keys; the C denotes a client message.
const SSH_AGENTC_REQUEST_IDENTITIES: u8 = 11;

/// Generic agent failure reply, which still proves the agent is responsive.
const SSH_AGENT_FAILURE: u8 = 5;

/// Agent reply containing its public keys and their comments.
const SSH_AGENT_IDENTITIES_ANSWER: u8 = 12;

/// Maximum probe reply payload size, bounding allocation before reading the reply.
const MAX_PROBE_REPLY_LEN: usize = 256 * 1024;

/// Validates the identities envelope without interpreting key types or comments.
fn valid_probe_reply(reply: &[u8]) -> bool {
    if reply == [SSH_AGENT_FAILURE] {
        return true;
    }

    // After the type byte, an identities answer starts with a big-endian u32 key count.
    let [SSH_AGENT_IDENTITIES_ANSWER, a, b, c, d, rest @ ..] = reply else {
        return false;
    };
    let count = u32::from_be_bytes([*a, *b, *c, *d]);
    let mut rest = rest;

    for _ in 0..count {
        // Each identity has two SSH strings: the public-key blob and its comment.
        // Each string starts with a big-endian u32 byte length, excluding the length field.
        for _ in 0..2 {
            let [a, b, c, d, contents @ ..] = rest else {
                return false;
            };
            let len = u32::from_be_bytes([*a, *b, *c, *d]) as usize;
            // Skip the string bytes, rejecting lengths beyond the remaining payload.
            let Some(tail) = contents.get(len..) else {
                return false;
            };
            rest = tail;
        }
    }

    // Extra bytes would contradict the declared identity count.
    rest.is_empty()
}

/// Tests if a `path` is listening and responds to the public keys list request.
async fn probe(path: &Path) -> Result<UnixStream> {
    let mut socket = UnixStream::connect(path).await?;

    // The request has a one-byte payload (type 11) and no additional fields.
    socket.write_all(&[0, 0, 0, 1, SSH_AGENTC_REQUEST_IDENTITIES]).await?;
    // read_u32 uses network byte order.  Every payload needs at least a type byte.
    let len = socket.read_u32().await? as usize;
    if !(1..=MAX_PROBE_REPLY_LEN).contains(&len) {
        return Err(error!(ErrorKind::InvalidData, "Invalid probe reply length"));
    }

    let mut reply = vec![0; len];
    socket.read_exact(&mut reply).await?;
    if !valid_probe_reply(&reply) {
        return Err(error!(ErrorKind::InvalidData, "Invalid probe reply"));
    }

    Ok(socket)
}

/// Races read-only probes and retains the winning connection, already drained of its reply.
pub(super) async fn select_socket(
    candidates: BTreeSet<PathBuf>,
    probe_timeout: Duration,
) -> Option<UnixStream> {
    let mut probes = JoinSet::new();
    for path in candidates {
        probes.spawn(async move {
            let result = timeout(probe_timeout, probe(&path)).await;
            // The outer result reports the deadline; the inner result reports probe I/O/validation.
            match result {
                Ok(Ok(socket)) => Some((path, socket)),
                Ok(Err(e)) => {
                    debug!("Ignoring unresponsive socket {}: {}", path.display(), e);
                    None
                }

                Err(_) => {
                    debug!("Ignoring unresponsive socket {}: probe timed out", path.display());
                    None
                }
            }
        });
    }

    let winner = loop {
        // Unwrap a completed task, its join result, and its optional responsive connection.
        match probes.join_next().await {
            Some(Ok(Some((path, socket)))) => {
                info!("Successfully opened responsive socket at {}", path.display());
                break Some(socket);
            }

            Some(Ok(None)) => (),
            Some(Err(e)) => debug!("Agent probe failed: {}", e),
            None => break None,
        }
    };

    // Draining also drops successful losers that finished before cancellation.
    probes.abort_all();
    while probes.join_next().await.is_some() {}

    winner
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::find::find_sockets;
    use std::fs;
    use tempfile::{TempDir, tempdir_in};
    use tokio::net::UnixListener;

    struct Sockets {
        dir: TempDir,
    }

    impl Sockets {
        /// Creates a uniquely named temporary directory for test sockets.
        fn new() -> Self {
            // Keep Unix socket paths short even when the system temp directory has a long path.
            let dir = tempdir_in("/tmp").unwrap();
            Self { dir }
        }

        /// Binds a test listener with the given name in the temporary directory.
        fn listen(&self, name: &str) -> (PathBuf, UnixListener) {
            let path = self.dir.path().join(name);
            let listener = UnixListener::bind(&path).unwrap();
            (path, listener)
        }
    }

    /// Accepts a connection and verifies its identities probe request.
    async fn accept_probe(listener: &UnixListener) -> UnixStream {
        let (mut stream, _) = listener.accept().await.unwrap();
        // Four bytes of framing plus type 11 (SSH_AGENTC_REQUEST_IDENTITIES).
        let mut request = [0; 5];
        stream.read_exact(&mut request).await.unwrap();
        assert_eq!(request, [0, 0, 0, 1, 11]);
        stream
    }

    #[test]
    fn test_valid_probe_reply_envelopes() {
        // These are payloads only: failure (5), empty identities (12), and one identity.
        assert!(valid_probe_reply(&[5]));
        assert!(valid_probe_reply(&[12, 0, 0, 0, 0]));

        // One key with a one-byte blob (42) and an empty comment; key contents are opaque here.
        assert!(valid_probe_reply(&[12, 0, 0, 0, 1, 0, 0, 0, 1, 42, 0, 0, 0, 0]));

        for reply in [
            &[][..],                               // Missing type.
            &[6],                                  // Generic success is not an identities answer.
            &[5, 0],                               // Failure must have no additional fields.
            &[12, 0, 0, 0],                        // Truncated key count.
            &[12, 0, 0, 0, 0, 0],                  // Trailing byte after zero keys.
            &[12, 0, 0, 0, 1],                     // One key declared, but none encoded.
            &[12, 255, 255, 255, 255],             // Maximum u32 key count with no keys.
            &[12, 0, 0, 0, 1, 255, 255, 255, 255], // Maximum u32 blob length with no blob.
        ] {
            assert!(!valid_probe_reply(reply));
        }
    }

    #[tokio::test]
    async fn test_select_socket_winner_and_cancellation() {
        for winner_first in [false, true] {
            let sockets = Sockets::new();
            let (a, a_listener) = sockets.listen("agent.a");
            let (b, b_listener) = sockets.listen("agent.b");
            let selection =
                tokio::spawn(select_socket(BTreeSet::from([a, b]), Duration::from_secs(10)));

            let (mut a, mut b) =
                tokio::join!(accept_probe(&a_listener), accept_probe(&b_listener),);
            let (winner, loser) = if winner_first { (&mut a, &mut b) } else { (&mut b, &mut a) };
            // A five-byte payload: identities answer (12) with a zero key count.
            winner.write_all(&[0, 0, 0, 5, 12, 0, 0, 0, 0]).await.unwrap();

            let mut selected =
                timeout(Duration::from_secs(2), selection).await.unwrap().unwrap().unwrap();
            assert_eq!(
                timeout(Duration::from_secs(2), loser.read_u8()).await.unwrap().unwrap_err().kind(),
                ErrorKind::UnexpectedEof,
            );

            // The winner keeps the probed connection, with no probe bytes left over.
            for message in [b"first".as_slice(), b"second".as_slice()] {
                selected.write_all(message).await.unwrap();
                let mut received = vec![0; message.len()];
                winner.read_exact(&mut received).await.unwrap();
                assert_eq!(received, message);
                winner.write_all(message).await.unwrap();
                selected.read_exact(&mut received).await.unwrap();
                assert_eq!(received, message);
            }
        }
    }

    #[tokio::test]
    async fn test_select_socket_simultaneous_replies() {
        let sockets = Sockets::new();
        let (a, a_listener) = sockets.listen("agent.a");
        let (b, b_listener) = sockets.listen("agent.b");
        let selection = tokio::spawn(select_socket(BTreeSet::from([a, b]), Duration::from_secs(2)));

        let (mut a, mut b) = tokio::join!(accept_probe(&a_listener), accept_probe(&b_listener),);
        // Both agents send a one-byte failure reply, which still proves responsiveness.
        a.write_all(&[0, 0, 0, 1, 5]).await.unwrap();
        b.write_all(&[0, 0, 0, 1, 5]).await.unwrap();

        let mut selected =
            timeout(Duration::from_secs(2), selection).await.unwrap().unwrap().unwrap();
        // An arbitrary marker identifies the winner after the probe reply has been drained.
        selected.write_all(&[42]).await.unwrap();

        let (a_reply, b_reply) =
            timeout(Duration::from_secs(2), async { tokio::join!(a.read_u8(), b.read_u8()) })
                .await
                .unwrap();
        match (a_reply, b_reply) {
            (Ok(42), Err(e)) | (Err(e), Ok(42)) => {
                assert_eq!(e.kind(), ErrorKind::UnexpectedEof);
            }

            replies => panic!("Unexpected replies: {replies:?}"),
        }
    }

    #[tokio::test]
    async fn test_select_socket_all_stalled() {
        let sockets = Sockets::new();
        let (path, listener) = sockets.listen("agent.silent");
        let selection =
            tokio::spawn(select_socket(BTreeSet::from([path]), Duration::from_millis(100)));

        let mut stream = accept_probe(&listener).await;
        assert!(timeout(Duration::from_secs(2), selection).await.unwrap().unwrap().is_none());
        assert_eq!(stream.read_u8().await.unwrap_err().kind(), ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn test_probe_bad_or_incomplete_replies() {
        for reply in [
            &[][..],                       // EOF before the length.
            &[0, 0],                       // Partial length.
            &[0, 0, 0, 5, 12],             // Five-byte payload declared, but only the type arrives.
            &[0, 0, 0, 0],                 // Zero length leaves no room for the type.
            &[0, 4, 0, 1],                 // 262145 bytes exceeds the 256 KiB limit.
            &[0, 0, 0, 1, 6],              // Generic success is not an identities answer.
            &[0, 0, 0, 5, 12, 0, 0, 0, 1], // One key declared, but none encoded.
        ] {
            let sockets = Sockets::new();
            let (path, listener) = sockets.listen("agent.bad");
            let selection =
                tokio::spawn(select_socket(BTreeSet::from([path]), Duration::from_millis(100)));

            let mut stream = accept_probe(&listener).await;
            stream.write_all(reply).await.unwrap();

            // Empty replies exercise immediate EOF; partial replies exercise the deadline.
            if reply.is_empty() {
                drop(stream);
            }
            assert!(timeout(Duration::from_secs(2), selection).await.unwrap().unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn test_probe_fragmented_failure_reply() {
        let sockets = Sockets::new();
        let (path, listener) = sockets.listen("agent.failure");
        let selection = tokio::spawn(select_socket(BTreeSet::from([path]), Duration::from_secs(2)));

        let mut stream = accept_probe(&listener).await;
        // Send the four-byte length and failure type separately to exercise partial reads.
        for byte in [0, 0, 0, 1, 5] {
            stream.write_all(&[byte]).await.unwrap();
            tokio::task::yield_now().await;
        }
        assert!(selection.await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_select_socket_no_candidates() {
        assert!(select_socket(BTreeSet::new(), Duration::from_secs(1)).await.is_none());
        assert!(
            select_socket(
                BTreeSet::from([PathBuf::from("/nonexistent-switcher-agent")]),
                Duration::from_secs(1),
            )
            .await
            .is_none()
        );
    }

    async fn do_select_socket_layouts_and_deduplication_test(
        name: &str,
        sockets: Sockets,
        dir: PathBuf,
    ) {
        let listener = UnixListener::bind(dir.join(name)).unwrap();
        let root = sockets.dir.path().to_owned();
        let selection = tokio::spawn(async move {
            let candidates =
                find_sockets(&[root.clone(), root.clone()], Some(&root), unsafe { libc::getuid() });
            select_socket(candidates, Duration::from_secs(2)).await
        });
        let mut stream = accept_probe(&listener).await;
        // Complete the probe with an empty identities answer (five-byte payload, type 12).
        stream.write_all(&[0, 0, 0, 5, 12, 0, 0, 0, 0]).await.unwrap();
        assert!(selection.await.unwrap().is_some());
        assert!(timeout(Duration::from_millis(50), listener.accept()).await.is_err());
    }

    #[tokio::test]
    async fn test_select_socket_layouts_and_deduplication_shared() {
        let sockets = Sockets::new();
        let dir = sockets.dir.path().join("ssh-session");
        fs::create_dir(&dir).unwrap();
        do_select_socket_layouts_and_deduplication_test("agent.test", sockets, dir).await;
    }

    #[tokio::test]
    async fn test_select_socket_layouts_and_deduplication_not_shared() {
        let sockets = Sockets::new();
        let dir = sockets.dir.path().to_owned();
        do_select_socket_layouts_and_deduplication_test("test.sshd.session", sockets, dir).await;
    }
}
