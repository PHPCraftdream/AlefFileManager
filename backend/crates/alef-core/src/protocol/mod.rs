// SPDX-License-Identifier: MIT OR Apache-2.0
//! Transport frames, credit-based flow control, and unary-call helpers (M1.2).
pub mod call;
pub mod credit;
pub mod frame;
pub mod transport;

/// Parses unary-call arguments from the transport header.
pub use call::{parse_args_header, Limits};
/// Credit-based flow control and byte chunking.
pub use credit::{chunk, CreditGate};
/// Transport frame codec.
pub use frame::{encode, Frame, FrameDecoder, FrameError};
