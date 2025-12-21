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
use std::io::Result;
use tokio::net::UnixStream;

// Forwards all request from the client to the agent, and all responses from the agent to the client.
pub(crate) async fn proxy_request(client: &mut UnixStream, agent: &mut UnixStream) -> Result<()> {
    // The buffer needs to be large enough to handle any one read or write by the client or
    // the agent.  Otherwise bad things will happen.
    //
    // TODO(jmerino): This could be improved but it's better to keep it simple.  In particular,
    // fixing this properly would require either spawning extra coroutines which, while they are
    // cheap, they are tricky to handle; or it would require a way to perform non-blocking reads
    // from the socket, which would then lead us to active polling which isn't too nice.
    let mut buf = [0; 4096];

    loop {
        trace!("Reading request from client");
        client.readable().await?;
        let n = client.try_read(&mut buf)?;
        trace!("Read {} bytes from client", n);
        if n == 0 {
            break;
        }

        trace!("Forwarding request of {} bytes to agent", n);
        agent.writable().await?;
        agent.try_write(&buf[0..n])?;

        trace!("Reading response from agent");
        agent.readable().await?;
        let n = agent.try_read(&mut buf)?;
        trace!("Read {} bytes from agent", n);
        if n > 0 {
            trace!("Forwarding response of {} bytes to agent", n);
            client.writable().await?;
            client.try_write(&buf[0..n])?;
        }
    }

    Ok(())
}
