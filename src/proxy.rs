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

//! Proxies traffic between two sockets.

use log::trace;
use std::io::{self, Result};
use tokio::io::Interest;
use tokio::net::UnixStream;
use tokio::select;

/// Default internal read buffer size.  This should be big enough to fit most reasonable agent
/// messages in one read/write, but the proxying logic can deal with partial messages.
const READ_BUF_SIZE: usize = 1024;

/// Handles one read from `stream` once the stream is readable.  Uses an internal buffer of
/// size `read_buf_size` and returns up to this many bytes.
async fn handle_read(stream: &mut UnixStream, read_buf_size: usize) -> Result<Vec<u8>> {
    let mut partial = vec![0; read_buf_size];
    match stream.try_read(&mut partial) {
        Ok(n) => {
            partial.truncate(n);
            Ok(partial)
        }
        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
            // The readiness event is a false positive.
            partial.truncate(0);
            Ok(partial)
        }
        Err(e) => Err(e),
    }
}

/// Handles one write to `stream` of all of `buf` once the stream is writable.
async fn handle_write(stream: &mut UnixStream, buf: &[u8]) -> Result<()> {
    let mut pos = 0;
    while pos < buf.len() {
        stream.writable().await?;
        match stream.try_write(&buf[pos..]) {
            Ok(n) => {
                pos += n;
                debug_assert!(pos <= buf.len());
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                // The readiness event is a false positive; try again.
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Forwards all request from the client to the agent and all responses from the agent to the client.
///
/// This is separate from `proxy_request` for testing purposes only as it allows configuring the
/// internal behavior of the proxying logic.
async fn proxy_request_internal(
    client: &mut UnixStream,
    agent: &mut UnixStream,
    read_buf_size: usize,
) -> Result<()> {
    let mut client_buf = vec![];
    let mut agent_buf = vec![];
    let mut client_done = false;
    let mut agent_done = false;
    while !(client_done && agent_done && agent_buf.is_empty() && client_buf.is_empty()) {
        select! {
            ready = client.ready(Interest::READABLE), if !client_done => {
                if ready?.is_readable() {
                    let partial = handle_read(client, read_buf_size).await?;
                    trace!(
                        "Read {} bytes from client; client buffer is now {}",
                        partial.len(), partial.len() + client_buf.len()
                    );
                    if partial.is_empty() {
                        trace!("Client socket is now half-closed");
                        client_done = true;
                    } else {
                        client_buf.extend_from_slice(&partial);
                    }
                }
            }

            ready = client.ready(Interest::WRITABLE), if !agent_buf.is_empty() => {
                if ready?.is_writable() {
                    trace!("Writing {} bytes to client", agent_buf.len());
                    handle_write(client, &mut agent_buf).await?;
                    agent_buf.clear();
                }
            }

            ready = agent.ready(Interest::READABLE), if !agent_done => {
                if ready?.is_readable() {
                    let partial = handle_read(agent, read_buf_size).await?;
                    trace!(
                        "Read {} bytes from agent; agent buffer is now {}",
                        partial.len(), partial.len() + agent_buf.len()
                    );
                    if partial.is_empty() {
                        trace!("Agent socket is now half-closed");
                        agent_done = true;
                    } else {
                        agent_buf.extend_from_slice(&partial);
                    }
                }
            }

            ready = agent.ready(Interest::WRITABLE), if !client_buf.is_empty() => {
                if ready?.is_writable() {
                    trace!("Writing {} bytes to agent", client_buf.len());
                    handle_write(agent, &mut client_buf).await?;
                    client_buf.clear();
                }
            }
        }
    }

    Ok(())
}

/// Forwards all request from the client to the agent and all responses from the agent to the client.
pub(crate) async fn proxy_request(client: &mut UnixStream, agent: &mut UnixStream) -> Result<()> {
    //proxy_request_internal(client, agent, READ_BUF_SIZE).await
    tokio::io::copy_bidirectional(client, agent).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads one message from `stream` in one go.
    async fn read_all(stream: &mut UnixStream, expected_len: usize) -> io::Result<Vec<u8>> {
        let mut buf = [0; 1024]; // Should be big enough for all test messages.
        let mut n = 0;
        while n < expected_len {
            stream.readable().await?;
            match stream.try_read(&mut buf[n..]) {
                Ok(n2) => n += n2,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => (),
                Err(e) => return Err(e),
            }
        }
        assert!(n < buf.len(), "Message reached buffer size; might be incomplete");
        Ok(buf[0..n].to_owned())
    }

    /// Writes all of `message` into `stream` in one go.
    async fn write_all(stream: &mut UnixStream, message: &[u8]) -> io::Result<()> {
        stream.writable().await?;
        let n = stream.try_write(message)?;
        assert_eq!(n, message.len(), "Failed to write message in one go");
        Ok(())
    }

    /// Performs a bidirectional proxying test with an internal read size of `read_buf_size`
    /// by sending `client_msg` to the agent and responding with `agent_msg` to the client.
    async fn do_bidi_test(
        read_buf_size: usize,
        client_msg: &str,
        agent_msg: &str,
    ) -> io::Result<()> {
        let (mut client_1, mut client_2) = UnixStream::pair()?;
        let (mut agent_1, mut agent_2) = UnixStream::pair()?;

        let proxy = tokio::spawn(async move {
            proxy_request_internal(&mut client_2, &mut agent_1, read_buf_size).await
        });

        let client_msg = client_msg.as_bytes();
        write_all(&mut client_1, client_msg).await?;
        assert_eq!(client_msg, read_all(&mut agent_2, client_msg.len()).await?);

        let agent_msg = agent_msg.as_bytes();
        write_all(&mut agent_2, agent_msg).await?;
        assert_eq!(agent_msg, read_all(&mut client_1, agent_msg.len()).await?);

        drop(client_1);
        proxy.await??;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_one_byte_at_a_time() -> io::Result<()> {
        do_bidi_test(1, "abcdefg", "hijklmn").await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_chunked() -> io::Result<()> {
        do_bidi_test(8, "request longer than eight bytes", "response longer than eight bytes").await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_one_chunk() -> io::Result<()> {
        do_bidi_test(1024, "request shorter than 1024 bytes", "response shorter than 1024 bytes")
            .await
    }
}
