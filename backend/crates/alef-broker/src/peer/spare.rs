// SPDX-License-Identifier: MIT OR Apache-2.0
//! Library for the `alef warm-spare` process (docs/stages/m11-warm-start.md,
//! «Архитектура реализации»): connect to the broker with the usual MAC +
//! `Hello` handshake, identify as a spare of one slot kind, then wait — with
//! explicit timeouts, no sleeps — for the single `Assign` of its lifetime.
//!
//! The spare speaks raw `SpareToBroker`/`BrokerToSpare` frames over the same
//! authenticated connection as a client (the transport routes non-`ClientMsg`
//! frames to the broker's `on_raw_frame`).

use std::io::{self, ErrorKind};
use std::time::Duration;

use crate::ipc::endpoint::Endpoint;
use crate::ipc::token::load_or_create_token;
use crate::ipc::{Client, ConnectError};
use crate::protocol::message::Launch;
use crate::protocol::warm::{
    decode_broker_to_spare, encode_spare_to_broker, BrokerToSpare, SlotKind, SpareHello,
    SpareToBroker,
};

use super::own_hello;

/// Explicit timeout for every spare frame (handshake reply, `Ack`,
/// `Ready`/`Declined` writes). Small on purpose: a broker that does not
/// answer within this is considered gone.
pub const SPARE_IO_TIMEOUT: Duration = Duration::from_secs(5);

/// The single assignment of a spare's lifetime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Assignment {
    /// Run this launch (re-apply `cwd`/`env` exactly, then the cold path).
    Launch(Launch),
}

/// Why [`Spare::wait_assignment`] gave up.
#[derive(Debug)]
pub enum WaitError {
    /// The broker told this spare to exit (unknown/retired spare, shutdown).
    Shutdown,
    /// No frame within the explicit timeout.
    Timeout,
    /// Connection error or an undecodable frame (fail-closed).
    Io(io::Error),
}

/// A connected spare role on an authenticated broker connection.
pub struct Spare {
    client: Client,
    /// The slot kind this spare registered as (echoed for logging/wiring).
    pub kind: SlotKind,
    /// The spare id this process was spawned with (argv bookkeeping only;
    /// the broker correlates by kind + pid).
    pub spare_id: u64,
}

impl Spare {
    /// Load the token per the documented path rules (next to the endpoint —
    /// Unix `<dir>/broker.token`, Windows `%LOCALAPPDATA%\alef\broker\<name>\`),
    /// connect, run the MAC + `Hello` handshake, then the spare handshake
    /// (`SpareToBroker::Hello` with this process's pid → `BrokerToSpare::Ack`).
    pub fn connect(endpoint: &Endpoint, kind: SlotKind, spare_id: u64) -> io::Result<Spare> {
        let (token, _created) = load_or_create_token(endpoint)?;
        let mut client =
            Client::connect_with_hello(endpoint, &token, own_hello()).map_err(connect_error)?;
        client.set_read_timeout(Some(SPARE_IO_TIMEOUT))?;
        let hello = SpareToBroker::Hello(SpareHello {
            kind,
            pid: std::process::id(),
            spare_id,
        });
        let body = encode_spare_to_broker(&hello).map_err(io::Error::other)?;
        client.send_raw(&body)?;
        let reply = client.recv_raw()?;
        match decode_broker_to_spare(&reply)
            .map_err(|e| io::Error::new(ErrorKind::InvalidData, e.to_string()))?
        {
            BrokerToSpare::Ack(_) => {}
            // The broker does not know this spare: exit quietly.
            BrokerToSpare::Shutdown(_) => {
                return Err(io::Error::new(
                    ErrorKind::ConnectionRefused,
                    "broker rejected the spare (unknown or retired)",
                ))
            }
            other => {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    format!("unexpected broker frame for a spare: {other:?}"),
                ))
            }
        }
        Ok(Spare {
            client,
            kind,
            spare_id,
        })
    }

    /// Warm-up finished: the broker may hand this spare out. Must be sent
    /// before the broker delivers any `Assign` for it.
    pub fn ready(&mut self) -> io::Result<()> {
        self.send_spare(&SpareToBroker::Ready(crate::protocol::warm::Ready))
    }

    /// Refuse the assignment BEFORE any irreversible action; the broker
    /// retires this spare (never hands it out again).
    pub fn decline(&mut self, reason: &str) -> io::Result<()> {
        self.send_spare(&SpareToBroker::Declined(
            crate::protocol::warm::SpareDeclined {
                reason: reason.to_string(),
            },
        ))
    }

    /// Wait for the one `Assign`, `Shutdown`, or the explicit timeout. The
    /// read timeout is set for the duration of the call and left in place.
    pub fn wait_assignment(&mut self, timeout: Duration) -> Result<Assignment, WaitError> {
        self.client
            .set_read_timeout(Some(timeout))
            .map_err(WaitError::Io)?;
        loop {
            match self.client.recv_raw() {
                Ok(body) => match decode_broker_to_spare(&body) {
                    Ok(BrokerToSpare::Assign(a)) => return Ok(Assignment::Launch(a.launch)),
                    Ok(BrokerToSpare::Shutdown(_)) => return Err(WaitError::Shutdown),
                    // A late duplicate of the connect-time Ack: not the
                    // assignment, keep waiting.
                    Ok(BrokerToSpare::Ack(_)) => continue,
                    Err(e) => {
                        return Err(WaitError::Io(io::Error::new(
                            ErrorKind::InvalidData,
                            e.to_string(),
                        )))
                    }
                },
                Err(e) if is_timeout(&e) => return Err(WaitError::Timeout),
                Err(e) => return Err(WaitError::Io(e)),
            }
        }
    }

    fn send_spare(&mut self, msg: &SpareToBroker) -> io::Result<()> {
        let body = encode_spare_to_broker(msg).map_err(io::Error::other)?;
        self.client.send_raw(&body)
    }
}

/// Map transport errors to I/O errors, preserving the caller-visible kind.
fn connect_error(e: ConnectError) -> io::Error {
    match e {
        ConnectError::NoBroker => io::Error::new(ErrorKind::NotFound, "broker is not running"),
        ConnectError::Incompatible(why) => io::Error::new(
            ErrorKind::ConnectionRefused,
            format!("incompatible broker: {why}"),
        ),
        ConnectError::AuthFailed => {
            io::Error::new(ErrorKind::PermissionDenied, "broker authentication failed")
        }
        ConnectError::Io(e) => e,
    }
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock)
}
