// SPDX-License-Identifier: MIT OR Apache-2.0
//! Client side of the broker transport: connect with timeouts, mutual MAC
//! handshake (server identity verified too), then send/recv with timeouts.
//!
//! Error mapping (see [`ConnectError`]): failure to open the entry point is
//! `NoBroker`; `Reject` with `Incompatible*` is `Incompatible`; any handshake
//! MAC/Hello/peer-identity failure is `AuthFailed`.

use std::io::{self, ErrorKind};
use std::time::Duration;

use crate::ipc::endpoint::Endpoint;
use crate::ipc::{write_frame, Conn, ConnectError, FrameReader, HANDSHAKE_CLIENT_REPLY_LEN};
use crate::protocol::auth::{client_verify_server, mac_client, NonceSource, RandomNonce, Token};
use crate::protocol::message::{
    decode_broker_msg, encode_client_msg, BrokerMsg, ClientMsg, Hello, HelloAck, RejectReason,
    PROTOCOL_VERSION,
};

/// Connect + handshake timeout for every client step.
pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

/// An authenticated, established client connection to the broker.
pub struct Client {
    conn: Box<dyn Conn>,
    frames: FrameReader,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The stream and the buffered frames are not printed.
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

impl Client {
    /// Connect with a minimal `Hello` (version only). Use
    /// [`Client::connect_with_hello`] when the caller knows its build and
    /// store identity.
    pub fn connect(ep: &Endpoint, token: &Token) -> Result<Client, ConnectError> {
        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            build: String::new(),
            alef_home: None,
            alef_app_home: None,
        };
        Client::connect_with_hello(ep, token, hello)
    }

    /// Connect, verify the server, run the mutual handshake, send `hello`,
    /// and wait for `HelloAck`.
    pub fn connect_with_hello(
        ep: &Endpoint,
        token: &Token,
        hello: Hello,
    ) -> Result<Client, ConnectError> {
        let mut conn = platform_connect(ep)?;
        conn.set_read_timeout(Some(CLIENT_TIMEOUT))
            .map_err(ConnectError::Io)?;
        conn.set_write_timeout(Some(CLIENT_TIMEOUT))
            .map_err(ConnectError::Io)?;

        // Verify the server's OS identity before sending any secret material.
        let own = crate::ipc::server::own_identity().map_err(ConnectError::Io)?;
        if conn.peer_identity().map_err(ConnectError::Io)? != own {
            return Err(ConnectError::AuthFailed);
        }

        // 1. server -> client: nonce_s
        let mut frames = FrameReader::new();
        let nonce_s_vec =
            read_exact_handshake(&mut frames, &mut conn, crate::protocol::auth::NONCE_LEN)?;
        let mut nonce_s = [0u8; crate::protocol::auth::NONCE_LEN];
        nonce_s.copy_from_slice(&nonce_s_vec);

        // 2. client -> server: nonce_c || mac_client
        let nonce_c = RandomNonce.generate();
        let mac = mac_client(token, &nonce_s, &nonce_c);
        let mut reply = Vec::with_capacity(HANDSHAKE_CLIENT_REPLY_LEN);
        reply.extend_from_slice(&nonce_c);
        reply.extend_from_slice(&mac);
        write_frame(&mut conn, &reply).map_err(ConnectError::Io)?;

        // 3. server -> client: mac_server
        let mac_s = read_exact_handshake(&mut frames, &mut conn, 32)?;
        let mut server_mac = [0u8; 32];
        server_mac.copy_from_slice(&mac_s);
        if !client_verify_server(token, &nonce_c, &nonce_s, &server_mac) {
            return Err(ConnectError::AuthFailed);
        }

        // 4. Hello -> HelloAck / Reject
        let body = encode_client_msg(&ClientMsg::Hello(hello))
            .map_err(|_| ConnectError::Incompatible("hello serialization failed"))?;
        write_frame(&mut conn, &body).map_err(ConnectError::Io)?;
        let ack = frames.read_frame(&mut conn).map_err(ConnectError::Io)?;
        match decode_broker_msg(&ack).map_err(|_| ConnectError::Incompatible("bad HelloAck"))? {
            BrokerMsg::HelloAck(HelloAck {
                protocol_version, ..
            }) => {
                if protocol_version != PROTOCOL_VERSION {
                    return Err(ConnectError::Incompatible("protocol version mismatch"));
                }
            }
            BrokerMsg::Reject(r) => return Err(reject_to_error(r.reason)),
            _ => return Err(ConnectError::Incompatible("expected HelloAck")),
        }
        Ok(Client { conn, frames })
    }

    /// Send one client message (framed, with the write timeout).
    pub fn send(&mut self, msg: &ClientMsg) -> io::Result<()> {
        let body = encode_client_msg(msg).map_err(io::Error::other)?;
        write_frame(&mut *self.conn, &body)
    }

    /// Receive one broker message (framed, with the read timeout). Frames
    /// completed together with a previous one are queued and returned in
    /// order (canonical `FrameDecoder::push` semantics).
    pub fn recv(&mut self) -> Result<BrokerMsg, ConnectError> {
        let body = self
            .frames
            .read_frame(&mut *self.conn)
            .map_err(ConnectError::Io)?;
        decode_broker_msg(&body).map_err(|_| ConnectError::Incompatible("bad broker message"))
    }
}

fn read_exact_handshake(
    frames: &mut FrameReader,
    conn: &mut dyn Conn,
    len: usize,
) -> Result<Vec<u8>, ConnectError> {
    frames
        .read_exact_frame(conn, len)
        .map_err(|e| match e.kind() {
            ErrorKind::InvalidData => ConnectError::AuthFailed,
            _ => ConnectError::Io(e),
        })
}

fn reject_to_error(reason: RejectReason) -> ConnectError {
    match reason {
        RejectReason::IncompatibleProtocol => ConnectError::Incompatible("protocol"),
        RejectReason::IncompatibleBuild => ConnectError::Incompatible("build"),
        RejectReason::StoreMismatch => ConnectError::Incompatible("store identity"),
        RejectReason::AuthFailed => ConnectError::AuthFailed,
        other => ConnectError::Incompatible(match other {
            RejectReason::TooBig => "too big",
            RejectReason::TooMany => "too many connections",
            RejectReason::RateLimited => "rate limited",
            RejectReason::ShuttingDown => "broker shutting down",
            RejectReason::BadRequest => "bad request",
            _ => "rejected",
        }),
    }
}

/// Platform connect; maps "cannot open the entry point" to `NoBroker`.
fn platform_connect(ep: &Endpoint) -> Result<Box<dyn Conn>, ConnectError> {
    #[cfg(unix)]
    {
        let stream = crate::ipc::unix::connect_with_timeout(&ep.unix_socket, CLIENT_TIMEOUT)
            .map_err(|e| match e.kind() {
                ErrorKind::NotFound
                | ErrorKind::ConnectionRefused
                | ErrorKind::PermissionDenied => ConnectError::NoBroker,
                _ => ConnectError::Io(e),
            })?;
        Ok(Box::new(stream))
    }
    #[cfg(windows)]
    {
        let own_sid = crate::ipc::windows::current_user_sid().map_err(ConnectError::Io)?;
        let handle = crate::ipc::windows::open_client(
            &ep.windows_pipe_wide(),
            CLIENT_TIMEOUT,
            Some(&own_sid),
        )
        .map_err(|e| match e.kind() {
            ErrorKind::NotFound => ConnectError::NoBroker,
            ErrorKind::PermissionDenied => ConnectError::AuthFailed,
            _ => ConnectError::Io(e),
        })?;
        let conn = crate::ipc::windows::PipeConn::new(handle, false).map_err(ConnectError::Io)?;
        Ok(Box::new(conn))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::auth::Token;

    fn test_token() -> Token {
        Token::new(vec![7u8; 32]).unwrap()
    }

    #[test]
    fn reject_mapping() {
        assert!(matches!(
            reject_to_error(RejectReason::IncompatibleProtocol),
            ConnectError::Incompatible(_)
        ));
        assert!(matches!(
            reject_to_error(RejectReason::AuthFailed),
            ConnectError::AuthFailed
        ));
        assert!(matches!(
            reject_to_error(RejectReason::ShuttingDown),
            ConnectError::Incompatible(_)
        ));
    }

    #[test]
    fn connect_to_missing_endpoint_is_no_broker() {
        let tmp = tempfile::tempdir().unwrap();
        let ep = Endpoint::for_session(tmp.path(), "no-broker");
        match Client::connect(&ep, &test_token()) {
            Err(ConnectError::NoBroker) => {}
            other => panic!("expected NoBroker, got {other:?}"),
        }
    }
}
