// SPDX-License-Identifier: MIT OR Apache-2.0
//! Launch logic of the generic `alef` runtime: command line, manifest, and the plan derived from it.
//! Kept apart from `main` so that it is testable without a window.
pub mod args;
pub mod consent;
pub mod permissions;
pub mod plan;
