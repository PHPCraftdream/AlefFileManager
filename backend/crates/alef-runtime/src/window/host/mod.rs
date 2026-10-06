// SPDX-License-Identifier: MIT OR Apache-2.0
//! What the UI thread does for the documents: all windows (open, close, the close request),
//! what a document may do to one window, the displays, and the events a window raises.
mod displays;
pub(super) mod events;
mod manage;
mod ops;
