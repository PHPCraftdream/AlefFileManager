// SPDX-License-Identifier: MIT OR Apache-2.0
//! Loading an application from its source, the plan derived from it, and the trust of its package.
pub mod plan;
pub mod source;
#[cfg(test)]
pub(crate) mod support;
pub mod trust;
pub mod window;
