// SPDX-License-Identifier: MIT OR Apache-2.0
//! `socket`: TCP connections (plain or TLS), TCP servers and UDP sockets, as `permissions.net.socket`
//! lets them be: `tcp:host:port` to connect, `udp:host:port` to send a datagram, `listen:host:port` to
//! take a port on this machine (for a server, and for a UDP socket, which also takes its port). A
//! connection is two streams with credit (what comes, what goes) and one resource that ends them both.
//! The right the user substituted is a dead network: a connection hangs until its timeout, a server
//! takes nobody, a datagram goes nowhere.
use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use alef_core::{
    ids::ResourceId,
    registry::{command::Reply, dispatch::Registry},
    session::{resources::Resource, streams::StreamWriter},
    AlefError, ErrorCode,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{net::UdpSocket, sync::Notify, task::JoinHandle};

mod tcp;
mod tls;
mod udp;

pub(in crate::net) use tcp::{open, Io, TlsOptions};

/// Where a server or a UDP socket listens when the page names no host: only this machine reaches it.
pub(in crate::net) const LOOPBACK: &str = "127.0.0.1";
/// How long a connection the user substituted hangs when it names no timeout.
const HANG: Duration = Duration::from_secs(30);
/// How long a socket that is closed has to write what the page sent before.
const CLOSE_GRACE: Duration = Duration::from_secs(2);

/// What a socket is for datagrams: not one of UDP, one of UDP, or the stand-in for one.
#[derive(Clone, Default)]
enum Udp {
    #[default]
    No,
    Real(Arc<UdpSocket>),
    StandIn,
}

/// Everything a socket resource holds: the tasks that move its bytes, the socket of UDP, and the
/// streams that a stand-in keeps open so that the page waits instead of failing.
#[derive(Default)]
pub(super) struct Socket {
    tasks: Mutex<Vec<JoinHandle<()>>>,
    /// The task that writes what the page sends: it is told to finish before it is cut off.
    writing: Mutex<Option<JoinHandle<()>>>,
    finish: Arc<Notify>,
    _held: Vec<StreamWriter>,
    udp: Udp,
}

impl Socket {
    pub(in crate::net) fn adopt(&self, tasks: impl IntoIterator<Item = JoinHandle<()>>) {
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(tasks);
    }

    fn adopt_writing(&self, task: JoinHandle<()>) {
        *self.writing.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
    }
}

impl Resource for Socket {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            // What the page wrote before it closed the socket goes out first, within reason.
            self.finish.notify_one();
            let writing = self
                .writing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(mut writing) = writing {
                if tokio::time::timeout(CLOSE_GRACE, &mut writing)
                    .await
                    .is_err()
                {
                    writing.abort();
                    let _ = writing.await;
                }
            }
            let tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|e| e.into_inner()));
            for task in &tasks {
                task.abort();
            }
            for task in tasks {
                let _ = task.await;
            }
        })
    }
}

/// What a server the user substituted is: nothing listens, and the stream of what would come stays open.
pub(in crate::net) fn standing(writer: StreamWriter) -> Socket {
    Socket {
        _held: vec![writer],
        ..Socket::default()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseArgs {
    socket: u64,
}

fn network(message: impl Into<String>) -> AlefError {
    AlefError::new(ErrorCode::Network, message.into())
}

fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

/// What a right is checked against: `tcp:host:port`, with an IPv6 literal in brackets.
pub(in crate::net) fn target(proto: &str, host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("{proto}:[{host}]:{port}")
    } else {
        format!("{proto}:{host}:{port}")
    }
}

fn address(address: SocketAddr) -> Value {
    json!({ "host": address.ip().to_string(), "port": address.port() })
}

/// What the user substituted for the network: nothing answers until the time is up.
pub(in crate::net) async fn dead(limit: Option<Duration>) -> AlefError {
    tokio::time::sleep(limit.unwrap_or(HANG)).await;
    AlefError::new(ErrorCode::Timeout, "the server did not answer in time")
}

pub(crate) fn register(registry: &mut Registry) -> Result<(), AlefError> {
    tcp::register(registry)?;
    udp::register(registry)?;
    registry
        .command::<CloseArgs>("socket.close")?
        .handler(|ctx, args| async move {
            let id = ResourceId(args.socket);
            ctx.resources().with_as::<Socket, _>(id, |_| ())?;
            ctx.resources().take(id)?.close().await;
            Ok(Reply::Json(Value::Null))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_has_the_protocol_the_host_and_the_port_and_an_ipv6_literal_in_brackets() {
        assert_eq!(target("tcp", "example.com", 443), "tcp:example.com:443");
        assert_eq!(target("listen", "127.0.0.1", 0), "listen:127.0.0.1:0");
        assert_eq!(target("udp", "::1", 53), "udp:[::1]:53");
        assert_eq!(target("udp", "[::1]", 53), "udp:[::1]:53");
    }

    #[test]
    fn an_address_tells_its_host_and_its_port() {
        let v4: SocketAddr = "127.0.0.1:80".parse().unwrap();
        let v6: SocketAddr = "[::1]:8080".parse().unwrap();
        assert_eq!(address(v4), json!({ "host": "127.0.0.1", "port": 80 }));
        assert_eq!(address(v6), json!({ "host": "::1", "port": 8080 }));
    }
}
