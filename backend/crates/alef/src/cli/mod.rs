// SPDX-License-Identifier: MIT OR Apache-2.0
//! The binary entry modules of `alef`: failure categories, signal handling, command dispatch, and the launch pipeline.
pub(crate) mod app_run;
pub(crate) mod dispatch;
pub(crate) mod failure;
pub(crate) mod signals;

/// The default window icon (the Alef logo) unless the application ships `icon.png`.
pub(crate) const DEFAULT_ICON: &[u8] = include_bytes!("../../../../assets/logo-32x32.png");
