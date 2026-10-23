// SPDX-License-Identifier: MIT OR Apache-2.0
//! Warm-start messages beyond the base client/broker exchange
//! (docs/stages/m11-warm-start.md, «Протокол»): relayed spare output and exit
//! to the client, the broker↔spare assignment handshake, and admin control.
//!
//! Same conventions as [`super::message`]: JSON bodies, serde tag `"type"`,
//! exact limits as constants. The spare connection reuses the existing
//! handshake (`Hello`/`HelloAck`) and [`Launch`]; these are only the messages
//! that follow it.

use serde::{Deserialize, Serialize};

use super::message::{Launch, RequestId};

/// Maximum bytes of spare stdout/stderr in one `Output` frame.
pub const MAX_OUTPUT_CHUNK: usize = 32 * 1024;

/// Which spare stream an `Output` chunk carries.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
}

/// A chunk of spare-process output, relayed by the broker to the client after
/// `Accepted`.
///
/// The broker never buffers unboundedly: while a chunk frame is queued for
/// the client connection, the broker stops draining the spare pipe, and the
/// OS pipe itself backpressures the spare process. Clients must keep reading
/// until `Exited`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Output {
    pub request_id: RequestId,
    pub stream: OutputStream,
    /// At most [`MAX_OUTPUT_CHUNK`] bytes.
    pub bytes: Vec<u8>,
}

/// Termination of the spare process: exactly one per request, the last
/// broker→client message of the launch.
///
/// Unix signal rule: when the process dies from signal `n`, `code` is
/// `128 + n` and `signal` is `Some(n)`; a normal exit has `signal: None`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Exited {
    pub request_id: RequestId,
    pub code: i32,
    pub signal: Option<i32>,
}

/// Wire-level slot kind (`pool::SlotKind` is the internal type; this is its
/// serde form for the wire).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum SlotKind {
    Windowed,
    Windowless,
}

/// Spare→broker: identifies a freshly connected spare after the usual MAC +
/// `Hello`/`HelloAck` handshake. The broker answers [`SpareAck`].
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct SpareHello {
    pub kind: SlotKind,
    pub pid: u32,
}

/// Broker→spare on connect.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct SpareAck;

/// Broker→spare: exactly one per connection. The spare re-applies `cwd`/`env`
/// exactly and runs the cold launch path on the pre-warmed window/GL.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Assign {
    pub launch: Launch,
}

/// Broker→spare: drop the warm-up and exit (never handed out).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Shutdown;

/// Spare→broker after `Assign`: warm-up finished. The broker must not hand
/// the process out (or report readiness) before this.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Ready;

/// Spare→broker instead of `Ready`: the spare refuses the assignment before
/// any irreversible action; the broker converts it to `Declined` for the
/// client.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct SpareDeclined {
    pub reason: String,
}

/// Admin (CLI `alef warm status`) → broker.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct AdminStatus;

/// One slot in [`AdminStatusReply`].
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct SlotStateEntry {
    pub kind: SlotKind,
    /// Human-readable pool slot state (`empty`, `initializing`, `ready`,
    /// `backoff`).
    pub state: String,
}

/// Broker → `alef warm status`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct AdminStatusReply {
    pub pid: u32,
    pub version: String,
    pub slots: Vec<SlotStateEntry>,
    /// Lifetime count of spares handed out.
    pub granted_total: u64,
    /// Currently live spare processes.
    pub running: u32,
}

/// Admin (CLI `alef warm stop`) → broker.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct AdminShutdown;

/// Broker → `alef warm stop`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct AdminAck;

/// Broker→client over the launch connection, after `Accepted`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WarmRelayMsg {
    Output(Output),
    Exited(Exited),
}

/// Spare→broker, after the handshake.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SpareToBroker {
    Hello(SpareHello),
    Ready(Ready),
    Declined(SpareDeclined),
}

/// Broker→spare, after `SpareHello`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrokerToSpare {
    Ack(SpareAck),
    Assign(Assign),
    Shutdown(Shutdown),
}

/// Admin requests → broker (dispatched alongside `ClientMsg`).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WarmAdminMsg {
    Status(AdminStatus),
    Shutdown(AdminShutdown),
}

/// Broker→admin replies.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WarmAdminReply {
    StatusReply(AdminStatusReply),
    Ack(AdminAck),
}

/// Serialize a relay message to a frame body (framing is `frame`'s job).
pub fn encode_warm_relay_msg(msg: &WarmRelayMsg) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(msg)
}

pub fn decode_warm_relay_msg(bytes: &[u8]) -> Result<WarmRelayMsg, serde_json::Error> {
    serde_json::from_slice(bytes)
}

/// Serialize a spare→broker message to a frame body.
pub fn encode_spare_to_broker(msg: &SpareToBroker) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(msg)
}

pub fn decode_spare_to_broker(bytes: &[u8]) -> Result<SpareToBroker, serde_json::Error> {
    serde_json::from_slice(bytes)
}

/// Serialize a broker→spare message to a frame body.
pub fn encode_broker_to_spare(msg: &BrokerToSpare) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(msg)
}

pub fn decode_broker_to_spare(bytes: &[u8]) -> Result<BrokerToSpare, serde_json::Error> {
    serde_json::from_slice(bytes)
}

/// Serialize an admin request to a frame body.
pub fn encode_warm_admin_msg(msg: &WarmAdminMsg) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(msg)
}

pub fn decode_warm_admin_msg(bytes: &[u8]) -> Result<WarmAdminMsg, serde_json::Error> {
    serde_json::from_slice(bytes)
}

/// Serialize an admin reply to a frame body.
pub fn encode_warm_admin_reply(msg: &WarmAdminReply) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(msg)
}

pub fn decode_warm_admin_reply(bytes: &[u8]) -> Result<WarmAdminReply, serde_json::Error> {
    serde_json::from_slice(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::message::OsBytes;

    fn sample_launch() -> Launch {
        Launch {
            request_id: "req-1".into(),
            target: OsBytes::from_os_string(std::ffi::OsStr::new("/apps/demo")),
            args: vec![],
            cwd: None,
            env: vec![],
            console: false,
        }
    }

    #[test]
    fn chunk_const_is_32kib() {
        assert_eq!(MAX_OUTPUT_CHUNK, 32768);
    }

    #[test]
    fn relay_round_trip() {
        for msg in [
            WarmRelayMsg::Output(Output {
                request_id: "r".into(),
                stream: OutputStream::Stdout,
                bytes: vec![0u8; MAX_OUTPUT_CHUNK],
            }),
            WarmRelayMsg::Output(Output {
                request_id: "r".into(),
                stream: OutputStream::Stderr,
                bytes: vec![1, 2, 3],
            }),
            WarmRelayMsg::Exited(Exited {
                request_id: "r".into(),
                code: 130,
                signal: Some(2),
            }),
            WarmRelayMsg::Exited(Exited {
                request_id: "r".into(),
                code: 0,
                signal: None,
            }),
        ] {
            let bytes = encode_warm_relay_msg(&msg).unwrap();
            assert_eq!(decode_warm_relay_msg(&bytes).unwrap(), msg);
        }
    }

    #[test]
    fn spare_round_trip() {
        for msg in [
            SpareToBroker::Hello(SpareHello {
                kind: SlotKind::Windowed,
                pid: 4242,
            }),
            SpareToBroker::Ready(Ready),
            SpareToBroker::Declined(SpareDeclined {
                reason: "no window".into(),
            }),
        ] {
            let bytes = encode_spare_to_broker(&msg).unwrap();
            assert_eq!(decode_spare_to_broker(&bytes).unwrap(), msg);
        }
        for msg in [
            BrokerToSpare::Ack(SpareAck),
            BrokerToSpare::Assign(Assign {
                launch: sample_launch(),
            }),
            BrokerToSpare::Shutdown(Shutdown),
        ] {
            let bytes = encode_broker_to_spare(&msg).unwrap();
            assert_eq!(decode_broker_to_spare(&bytes).unwrap(), msg);
        }
    }

    #[test]
    fn admin_round_trip() {
        for msg in [
            WarmAdminMsg::Status(AdminStatus),
            WarmAdminMsg::Shutdown(AdminShutdown),
        ] {
            let bytes = encode_warm_admin_msg(&msg).unwrap();
            assert_eq!(decode_warm_admin_msg(&bytes).unwrap(), msg);
        }
        for msg in [
            WarmAdminReply::StatusReply(AdminStatusReply {
                pid: 1,
                version: "0.1.0-test".into(),
                slots: vec![SlotStateEntry {
                    kind: SlotKind::Windowless,
                    state: "ready".into(),
                }],
                granted_total: 7,
                running: 2,
            }),
            WarmAdminReply::Ack(AdminAck),
        ] {
            let bytes = encode_warm_admin_reply(&msg).unwrap();
            assert_eq!(decode_warm_admin_reply(&bytes).unwrap(), msg);
        }
    }

    #[test]
    fn bodies_are_tagged_json() {
        let v: serde_json::Value = serde_json::from_slice(
            &encode_warm_relay_msg(&WarmRelayMsg::Exited(Exited {
                request_id: "r".into(),
                code: 1,
                signal: None,
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(v["type"], serde_json::Value::String("exited".into()));
        let v: serde_json::Value = serde_json::from_slice(
            &encode_spare_to_broker(&SpareToBroker::Hello(SpareHello {
                kind: SlotKind::Windowed,
                pid: 1,
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(v["type"], serde_json::Value::String("hello".into()));
    }
}
