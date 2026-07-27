//! SSH port forwarding helpers.
//!
//! Two forwarding modes are supported:
//!
//! * **Local / direct-tcpip** – the client opens a `direct-tcpip` channel and
//!   the server connects to the requested destination, proxying bytes in both
//!   directions (`channel_open_direct_tcpip`).
//!
//! * **Remote / tcpip-forward** – the client asks the server to listen on a
//!   local address (`tcpip_forward`); incoming connections are forwarded back
//!   to the client, which opens a `forwarded-tcpip` channel for each one.

use tokio::net::TcpStream;

use russh::server::Msg;
use russh::Channel;

/// Bidirectionally proxy data between an SSH channel and a TCP stream until
/// either side closes or an error occurs.
pub async fn proxy(channel: Channel<Msg>, tcp: TcpStream) {
    let mut ssh = channel.into_stream();
    let mut tcp = tcp;
    match tokio::io::copy_bidirectional(&mut ssh, &mut tcp).await {
        Ok((from_ssh, from_tcp)) => {
            tracing::debug!(from_ssh, from_tcp, "port forward closed");
        }
        Err(e) => {
            tracing::warn!(?e, "port forward error");
        }
    }
}
