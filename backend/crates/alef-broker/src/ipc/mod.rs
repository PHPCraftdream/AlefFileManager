// SPDX-License-Identifier: MIT OR Apache-2.0
//! Blocking std-only transport of the warm-start broker
//! (docs/stages/m11-warm-start.md, «IPC, полномочия и передача управления»).
//!
//! Plain `std::net`/`std::os` I/O plus `std::thread`; no async runtime. Every
//! read and write has a timeout. Platform entry points, permissions, peer
//! credential checks and the token file live here; the wire format lives in
//! `crate::protocol`.
//!
//! Handshake framing (on top of `protocol::frame`): server sends a frame with
//! `nonce_s` (32 bytes); client replies `nonce_c || mac_client` (64 bytes);
//! server replies `mac_server` (32 bytes). The first framed message after the
//! MAC exchange is `ClientMsg::Hello`, answered by `BrokerMsg::HelloAck` or
//! `BrokerMsg::Reject`.
//!
//! Not implemented here (left to later slices): console stream handoff
//! (SCM_RIGHTS / DuplicateHandle) and any real message handling.

pub mod client;
pub mod endpoint;
pub mod server;
pub mod token;
#[cfg(unix)]
pub mod unix;
#[cfg(windows)]
pub mod windows;

use std::error::Error;
use std::fmt;
use std::io::{self, ErrorKind, Read, Write};
use std::time::Duration;

use crate::protocol::auth::NONCE_LEN;
use crate::protocol::frame::{encode_frame, FrameDecoder, FrameError};
use crate::protocol::message::{encode_broker_msg, BrokerMsg};

pub use client::Client;
pub use endpoint::{Endpoint, EndpointError};
pub use server::{ExpectedIdentity, Handler, Server};
pub use token::load_or_create_token;

/// A bidirectional byte stream to (or from) a broker peer, with timeouts and
/// an authenticated peer identity.
pub trait Conn: Read + Write + Send {
    /// Set the read timeout; `None` blocks indefinitely.
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()>;
    /// Set the write timeout; `None` blocks indefinitely.
    fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()>;
    /// OS-level identity of the peer (uid on Unix, SID + session on Windows).
    fn peer_identity(&self) -> io::Result<PeerIdentity>;
}

/// Identity of an IPC peer as attested by the OS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerIdentity {
    UnixUid(u32),
    WindowsSid { sid: String, session_id: u32 },
}

/// Errors when establishing a connection to the broker.
#[derive(Debug)]
pub enum ConnectError {
    /// No broker is listening at the entry point.
    NoBroker,
    /// The broker rejected the connection as incompatible.
    Incompatible(&'static str),
    /// Mutual authentication failed (wrong token or wrong peer identity).
    AuthFailed,
    Io(io::Error),
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectError::NoBroker => write!(f, "broker is not running"),
            ConnectError::Incompatible(why) => write!(f, "incompatible broker: {why}"),
            ConnectError::AuthFailed => write!(f, "broker authentication failed"),
            ConnectError::Io(e) => write!(f, "broker connection I/O error: {e}"),
        }
    }
}

impl Error for ConnectError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ConnectError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for ConnectError {
    fn from(e: io::Error) -> Self {
        ConnectError::Io(e)
    }
}

/// A platform listener that accepts broker connections.
///
/// `Sync` so a [`Server`] can be shared across threads.
pub trait Listener: Send + Sync {
    /// Block until a peer connects; the returned `Conn` is the accepted side.
    fn accept(&self) -> io::Result<Box<dyn Conn>>;
    /// Unblock a pending `accept` by connecting to our own entry point.
    fn wake(&self);
}

/// Sending side used by the broker's [`Handler`] to answer a message.
pub trait ReplyTo {
    fn send(&mut self, msg: &BrokerMsg) -> io::Result<()>;
}

/// Write one framed message body to a stream.
pub fn write_frame<W: Write + ?Sized>(w: &mut W, body: &[u8]) -> io::Result<()> {
    let bytes = encode_frame(body).map_err(io::Error::other)?;
    w.write_all(&bytes)
}

/// `Box<C>` forwards to `C` (std already forwards `Read`/`Write`), so a
/// `Box<dyn Conn>` is itself a `Conn` and can be coerced to `&mut dyn Conn`.
impl<C: Conn + ?Sized> Conn for Box<C> {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        (**self).set_read_timeout(timeout)
    }
    fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        (**self).set_write_timeout(timeout)
    }
    fn peer_identity(&self) -> io::Result<PeerIdentity> {
        (**self).peer_identity()
    }
}

/// Every `Conn` can answer broker messages: framing is the same both ways.
impl<C: Conn + ?Sized> ReplyTo for C {
    fn send(&mut self, msg: &BrokerMsg) -> io::Result<()> {
        let body = encode_broker_msg(msg).map_err(io::Error::other)?;
        write_frame(self, &body)
    }
}
/// Incremental frame reader over a [`Conn`].
///
/// Canonical `FrameDecoder::push` semantics: it returns EVERY frame completed
/// by the pushed bytes, including one ending exactly at the end of the push.
/// One read may therefore complete several frames; this reader queues them
/// and hands them out in arrival order, never dropping or duplicating.
pub struct FrameReader {
    decoder: FrameDecoder,
    pending: std::collections::VecDeque<Vec<u8>>,
}

impl Default for FrameReader {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameReader {
    pub fn new() -> Self {
        FrameReader {
            decoder: FrameDecoder::new(),
            pending: std::collections::VecDeque::new(),
        }
    }

    /// Read one complete frame body, draining the pending queue first.
    pub fn read_frame(&mut self, conn: &mut dyn Conn) -> io::Result<Vec<u8>> {
        let mut buf = [0u8; 8192];
        loop {
            if let Some(body) = self.pending.pop_front() {
                return Ok(body);
            }
            let n = conn.read(&mut buf)?;
            if n == 0 {
                return Err(io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "peer closed mid-frame",
                ));
            }
            match self.decoder.push(&buf[..n]) {
                Ok(bodies) => {
                    let mut it = bodies.into_iter();
                    if let Some(first) = it.next() {
                        self.pending.extend(it);
                        return Ok(first);
                    }
                }
                Err(FrameError::TooLarge { .. }) => {
                    return Err(io::Error::new(
                        ErrorKind::ConnectionReset,
                        "frame exceeds the hard cap; stream unsynchronized",
                    ));
                }
                Err(e) => {
                    return Err(io::Error::new(ErrorKind::InvalidData, e.to_string()));
                }
            }
        }
    }

    /// Read one handshake frame of an exact length; other lengths are
    /// authentication failures (wrong protocol, never resynchronized).
    pub fn read_exact_frame(&mut self, conn: &mut dyn Conn, len: usize) -> io::Result<Vec<u8>> {
        let body = self.read_frame(conn)?;
        if body.len() != len {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "handshake frame has wrong length",
            ));
        }
        Ok(body)
    }
}

/// Length of the client's handshake reply: `nonce_c || mac_client`.
pub const HANDSHAKE_CLIENT_REPLY_LEN: usize = NONCE_LEN + 32;

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock Conn feeding canned chunks; canonical push semantics test.
    struct MockConn {
        chunks: Vec<Vec<u8>>,
        i: usize,
    }
    impl Read for MockConn {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.i >= self.chunks.len() {
                return Ok(0);
            }
            let c = &self.chunks[self.i];
            self.i += 1;
            let n = buf.len().min(c.len());
            buf[..n].copy_from_slice(&c[..n]);
            Ok(n)
        }
    }
    impl Write for MockConn {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Conn for MockConn {
        fn set_read_timeout(&mut self, _: Option<Duration>) -> io::Result<()> {
            Ok(())
        }
        fn set_write_timeout(&mut self, _: Option<Duration>) -> io::Result<()> {
            Ok(())
        }
        fn peer_identity(&self) -> io::Result<PeerIdentity> {
            unreachable!()
        }
    }

    fn frame(body: &[u8]) -> Vec<u8> {
        encode_frame(body).unwrap()
    }

    #[test]
    fn all_frames_per_push_are_queued_in_order() {
        // One chunk completes THREE frames; none may be lost or duplicated.
        let data = [frame(b"a"), frame(b"bb"), frame(b"ccc")].concat();
        let mut conn = MockConn {
            chunks: vec![data],
            i: 0,
        };
        let mut fr = FrameReader::new();
        assert_eq!(fr.read_frame(&mut conn).unwrap(), b"a");
        assert_eq!(fr.read_frame(&mut conn).unwrap(), b"bb");
        assert_eq!(fr.read_frame(&mut conn).unwrap(), b"ccc");
    }

    #[test]
    fn frame_split_across_pushes_is_completed_once() {
        let data = frame(b"hello");
        let mut conn = MockConn {
            chunks: vec![data[..3].to_vec(), data[3..].to_vec()],
            i: 0,
        };
        let mut fr = FrameReader::new();
        // Frame ending exactly at the end of a push is returned immediately.
        assert_eq!(fr.read_frame(&mut conn).unwrap(), b"hello");
        assert!(matches!(
            fr.read_frame(&mut conn),
            Err(e) if e.kind() == ErrorKind::UnexpectedEof
        ));
    }
}
