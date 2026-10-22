// SPDX-License-Identifier: MIT OR Apache-2.0
//! Wire messages of the warm-start broker.
//!
//! Bodies are JSON (via `serde_json`): frames are small and few, and
//! human-debuggable logs matter more than compactness. OS-native values
//! (exact Unix bytes / exact Windows UTF-16 code units) are carried in the
//! tagged [`OsBytes`] type, never as lossy JSON strings.

use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};

/// Current wire protocol version.
pub const PROTOCOL_VERSION: u32 = 1;

/// Client-generated identifier, unique per logical launch; compared by
/// equality and persisted in the request log so a `Query` after a reconnect
/// can resolve it.
pub type RequestId = String;

/// Exact OS-native string bytes, carried losslessly across the wire.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "platform", content = "units", rename_all = "lowercase")]
pub enum OsBytes {
    Unix(Vec<u8>),
    /// Windows UTF-16 code units, exactly as `OsString` extends them
    /// (unpaired surrogates preserved, WTF-8 internally).
    Windows(Vec<u16>),
}

/// A value originating on another platform cannot be used as an OS string on
/// this one; no lossy conversion is ever performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrossPlatform;

impl std::fmt::Display for CrossPlatform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "value from another platform cannot be used as an OS string here"
        )
    }
}

impl std::error::Error for CrossPlatform {}

impl OsBytes {
    /// Capture the exact bytes/units of an OS string on this platform.
    pub fn from_os_string(s: &OsStr) -> OsBytes {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            OsBytes::Unix(s.as_bytes().to_vec())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            OsBytes::Windows(s.encode_wide().collect())
        }
    }

    /// Convert back to an OS string; errors instead of converting lossily if
    /// the value came from the other platform.
    pub fn into_os_string(self) -> Result<OsString, CrossPlatform> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            match self {
                OsBytes::Unix(bytes) => Ok(OsString::from_vec(bytes)),
                OsBytes::Windows(_) => Err(CrossPlatform),
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            match self {
                OsBytes::Windows(units) => Ok(OsString::from_wide(&units)),
                OsBytes::Unix(_) => Err(CrossPlatform),
            }
        }
    }
}

/// One environment variable change. `value: None` means the client
/// explicitly REMOVES the variable — absence is carried exactly, distinct
/// from an empty value.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct EnvChange {
    pub key: OsBytes,
    pub value: Option<OsBytes>,
}

/// Client hello: version and build identity, plus the exact values (or
/// absence) of the store-identity variables. Relative paths are NOT resolved
/// by the protocol; the broker resolves them against the caller's cwd.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Hello {
    pub protocol_version: u32,
    pub build: String,
    /// `ALEF_HOME`: `Some` = variable set to exactly this value,
    /// `None` = variable absent.
    pub alef_home: Option<OsBytes>,
    /// `ALEF_APP_HOME`, same semantics.
    pub alef_app_home: Option<OsBytes>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct HelloAck {
    pub protocol_version: u32,
    pub build: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    IncompatibleProtocol,
    IncompatibleBuild,
    StoreMismatch,
    AuthFailed,
    TooBig,
    TooMany,
    RateLimited,
    ShuttingDown,
    BadRequest,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedReason {
    Devtools,
    Service,
    Session0,
    SingleInstanceArbitration,
    NoGraphicsSession,
    Relaunch,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum DeclineReason {
    NoSpare,
    Busy,
    Unsupported(UnsupportedReason),
    ValidationFailed,
}

/// Outcome of a launch as known by the broker.
///
/// - `Unknown`: the broker never saw this request id. Safe to cold-launch
///   only when the client has PROVEN the broker never accepted it (e.g. the
///   request log, or an authoritative absence of the id on a live broker).
///   Never treat a connection failure as `Unknown`.
/// - `UnknownOutcome`: the broker restarted (or its log is incomplete), so
///   the request may or may not have been accepted and the worker may be
///   running. The client must NEVER blind-cold-duplicate on this: ask the
///   user / surface an explicit action instead.
/// - `Accepted` / `HandedOff`: a process exists or existed for this id.
/// - `Failed`: the broker rejected or failed the launch after acceptance.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum LaunchState {
    Unknown,
    UnknownOutcome,
    Accepted,
    HandedOff,
    Failed(String),
}

/// Launch target: exact OS values, no shell, no lossy text. Relative paths
/// are resolved by the broker against the caller's `cwd`, then re-validated.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Launch {
    pub request_id: RequestId,
    pub target: OsBytes,
    pub args: Vec<OsBytes>,
    pub cwd: Option<OsBytes>,
    pub env: Vec<EnvChange>,
    pub console: bool,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Query {
    pub request_id: RequestId,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello(Hello),
    Launch(Launch),
    Query(Query),
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub struct Reject {
    pub reason: RejectReason,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub struct Accepted {
    pub request_id: RequestId,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub struct Declined {
    pub request_id: RequestId,
    pub reason: DeclineReason,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub struct Status {
    pub request_id: RequestId,
    pub state: LaunchState,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrokerMsg {
    HelloAck(HelloAck),
    Reject(Reject),
    Accepted(Accepted),
    Declined(Declined),
    Status(Status),
}

/// Serialize a client message to a frame body (framing itself is `frame`'s job).
pub fn encode_client_msg(msg: &ClientMsg) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(msg)
}

pub fn decode_client_msg(bytes: &[u8]) -> Result<ClientMsg, serde_json::Error> {
    serde_json::from_slice(bytes)
}

/// Serialize a broker message to a frame body.
pub fn encode_broker_msg(msg: &BrokerMsg) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(msg)
}

pub fn decode_broker_msg(bytes: &[u8]) -> Result<BrokerMsg, serde_json::Error> {
    serde_json::from_slice(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_round_trip() {
        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            build: "0.1.0-test".into(),
            alef_home: Some(OsBytes::from_os_string(OsStr::new("/home/u/.alef"))),
            alef_app_home: None,
        };
        let bytes = encode_client_msg(&ClientMsg::Hello(hello.clone())).unwrap();
        match decode_client_msg(&bytes).unwrap() {
            ClientMsg::Hello(decoded) => assert_eq!(decoded, hello),
            other => panic!("wrong message: {other:?}"),
        }
    }

    #[test]
    fn reject_reason_tags() {
        for (reason, tag) in [
            (RejectReason::IncompatibleProtocol, "incompatible_protocol"),
            (RejectReason::StoreMismatch, "store_mismatch"),
            (RejectReason::ShuttingDown, "shutting_down"),
        ] {
            let v = serde_json::to_value(&reason).unwrap();
            assert_eq!(v, serde_json::Value::String(tag.into()));
        }
    }

    #[test]
    fn launch_round_trip() {
        let launch = Launch {
            request_id: "req-1".into(),
            target: OsBytes::from_os_string(OsStr::new("/apps/demo")),
            args: vec![
                OsBytes::from_os_string(OsStr::new("--flag")),
                OsBytes::from_os_string(OsStr::new("väl")),
            ],
            cwd: Some(OsBytes::from_os_string(OsStr::new("/tmp"))),
            env: vec![
                EnvChange {
                    key: OsBytes::from_os_string(OsStr::new("FOO")),
                    value: Some(OsBytes::from_os_string(OsStr::new("bar"))),
                },
                EnvChange {
                    key: OsBytes::from_os_string(OsStr::new("OLD")),
                    value: None,
                },
            ],
            console: true,
        };
        let bytes = encode_client_msg(&ClientMsg::Launch(launch.clone())).unwrap();
        match decode_client_msg(&bytes).unwrap() {
            ClientMsg::Launch(decoded) => assert_eq!(decoded, launch),
            other => panic!("wrong message: {other:?}"),
        }
    }

    #[test]
    fn launch_state_round_trip() {
        for state in [
            LaunchState::Unknown,
            LaunchState::UnknownOutcome,
            LaunchState::Accepted,
            LaunchState::HandedOff,
            LaunchState::Failed("boom".into()),
        ] {
            let msg = BrokerMsg::Status(Status {
                request_id: "r".into(),
                state: state.clone(),
            });
            let bytes = encode_broker_msg(&msg).unwrap();
            match decode_broker_msg(&bytes).unwrap() {
                BrokerMsg::Status(s) => assert_eq!(s.state, state),
                other => panic!("wrong message: {other:?}"),
            }
        }
    }

    // On `UnknownOutcome` no test may assert a cold fallback: per the design
    // doc (docs/stages/m11-warm-start.md) the client must never blind-cold-
    // duplicate after an undefined outcome — only `Unknown`, proven by the
    // client, permits a fresh launch. This test exists to document that rule
    // next to the type.

    #[test]
    fn unknown_outcome_round_trip() {
        let msg = BrokerMsg::Status(Status {
            request_id: "r".into(),
            state: LaunchState::UnknownOutcome,
        });
        let bytes = encode_broker_msg(&msg).unwrap();
        match decode_broker_msg(&bytes).unwrap() {
            BrokerMsg::Status(s) => assert_eq!(s.state, LaunchState::UnknownOutcome),
            other => panic!("wrong message: {other:?}"),
        }
    }

    #[test]
    fn garbage_decode_is_error_not_panic() {
        assert!(decode_client_msg(b"\xff\xfe garbage {").is_err());
        assert!(decode_broker_msg(b"[]").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn os_bytes_lossless_non_utf8_unix() {
        use std::os::unix::ffi::OsStrExt;
        // Invalid UTF-8 sequence (truncated emoji + stray byte).
        let raw: &[u8] = &[0x66, 0xF0, 0x9F];
        let os = OsStr::from_bytes(raw);
        let ob = OsBytes::from_os_string(os);
        assert_eq!(ob, OsBytes::Unix(raw.to_vec()));
        assert_eq!(ob.into_os_string().unwrap().as_os_str().as_bytes(), raw);
    }

    #[cfg(unix)]
    #[test]
    fn windows_variant_rejected_on_unix() {
        let err = OsBytes::Windows(vec![0x61]).into_os_string();
        assert!(err.is_err());
    }

    #[cfg(windows)]
    #[test]
    fn os_bytes_lossless_unpaired_surrogate_windows() {
        use std::os::windows::ffi::OsStringExt;
        // 'a' + unpaired high surrogate.
        let units: Vec<u16> = vec![0x61, 0xD800];
        let os = OsString::from_wide(&units);
        let ob = OsBytes::from_os_string(os.as_os_str());
        assert_eq!(ob, OsBytes::Windows(units.clone()));
        assert_eq!(
            ob.into_os_string()
                .unwrap()
                .encode_wide()
                .collect::<Vec<_>>(),
            units
        );
    }

    #[cfg(windows)]
    #[test]
    fn unix_variant_rejected_on_windows() {
        let err = OsBytes::Unix(vec![0x61]).into_os_string();
        assert!(err.is_err());
    }

    #[test]
    fn client_msg_body_is_tagged_json() {
        let bytes = encode_client_msg(&ClientMsg::Query(Query {
            request_id: "q".into(),
        }))
        .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["type"], serde_json::Value::String("query".into()));
        let bytes = encode_broker_msg(&BrokerMsg::HelloAck(HelloAck {
            protocol_version: 1,
            build: "b".into(),
        }))
        .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["type"], serde_json::Value::String("hello_ack".into()));
    }
}
