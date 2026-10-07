// SPDX-License-Identifier: MIT OR Apache-2.0
//! UDP: a socket bound to a port of this machine (the right `listen:`), the datagrams that arrive as
//! frames of a stream (the sender, and the bytes in base64), the datagrams that go with `socket.send`
//! (the right `udp:` to the place they go to).
use std::{io, sync::Arc};

use alef_core::{
    ids::ResourceId,
    registry::{command::Reply, dispatch::Registry},
    security::{consent::Decision, permissions::Permission},
    session::streams::StreamWriter,
    AlefError,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::UdpSocket;

use super::{address, invalid, network, target, Socket, Udp, LOOPBACK};
use crate::json;

/// The most a datagram of UDP over IPv4 carries.
const MAX_DATAGRAM: usize = 65507;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BindArgs {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    port: u16,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SendArgs {
    socket: u64,
    host: String,
    port: u16,
}

/// Hands every datagram that arrives to the page, with whom it came from.
async fn receive_pump(socket: Arc<UdpSocket>, writer: StreamWriter) {
    let mut buffer = vec![0_u8; MAX_DATAGRAM + 1];
    loop {
        match socket.recv_from(&mut buffer).await {
            Ok((count, from)) => {
                let datagram = json!({
                    "host": from.ip().to_string(),
                    "port": from.port(),
                    "data": STANDARD.encode(&buffer[..count]),
                });
                if writer.send_json(datagram).await.is_err() {
                    return;
                }
            }
            // Windows tells a socket that a datagram it sent found nobody there; the socket goes on.
            Err(error) if error.kind() == io::ErrorKind::ConnectionReset => {}
            Err(error) => {
                writer.error(network(format!("the socket broke: {error}")));
                return;
            }
        }
    }
}

pub(super) fn register(registry: &mut Registry) -> Result<(), AlefError> {
    registry
        .command::<BindArgs>("socket.udp")?
        .permission(Permission::NetSocket, |args| {
            Some(target(
                "listen",
                args.host.as_deref().unwrap_or(LOOPBACK),
                args.port,
            ))
        })
        .substitutes()
        .handler(|ctx, args| async move {
            let host = args.host.as_deref().unwrap_or(LOOPBACK);
            let (writer, messages) = ctx.streams().open_outgoing();
            if ctx.decision() == Decision::Substitute {
                // A port that was never taken: what is sent to it goes nowhere, nothing arrives.
                let standing = Socket {
                    _held: vec![writer],
                    udp: Udp::StandIn,
                    ..Socket::default()
                };
                let id = ctx.resources().insert(Box::new(standing))?;
                let local = json!({ "host": host, "port": args.port });
                return json(
                    &json!({ "socket": id.0, "messages": messages.0, "localAddress": local }),
                );
            }
            let socket = Arc::new(UdpSocket::bind((host, args.port)).await?);
            let local = socket.local_addr()?;
            let bound = Socket {
                udp: Udp::Real(socket.clone()),
                ..Socket::default()
            };
            let id = ctx.resources().insert(Box::new(bound))?;
            let task = tokio::spawn(receive_pump(socket, writer));
            ctx.resources()
                .with_as::<Socket, _>(id, |socket| socket.adopt([task]))?;
            json(&json!({ "socket": id.0, "messages": messages.0, "localAddress": address(local) }))
        })?;

    registry
        .command::<SendArgs>("socket.send")?
        .permission(Permission::NetSocket, |args| {
            Some(target("udp", &args.host, args.port))
        })
        .substitutes()
        .handler(|ctx, args| async move {
            let id = ResourceId(args.socket);
            let udp = ctx
                .resources()
                .with_as::<Socket, _>(id, |socket| socket.udp.clone())?;
            let data = ctx.body().cloned().unwrap_or_default();
            if data.len() > MAX_DATAGRAM {
                return Err(invalid("a datagram is at most 65507 bytes"));
            }
            let socket = match udp {
                Udp::No => return Err(invalid("this socket is not one of UDP")),
                Udp::StandIn => return Ok(Reply::Json(Value::Null)),
                Udp::Real(socket) => socket,
            };
            if ctx.decision() == Decision::Substitute {
                return Ok(Reply::Json(Value::Null));
            }
            let to = tokio::net::lookup_host((args.host.as_str(), args.port))
                .await
                .map_err(|error| network(format!("cannot find the address: {error}")))?
                .next()
                .ok_or_else(|| network("the name has no address"))?;
            socket
                .send_to(&data, to)
                .await
                .map_err(|error| network(format!("the datagram was not sent: {error}")))?;
            Ok(Reply::Json(Value::Null))
        })
}
