// SPDX-License-Identifier: MIT OR Apache-2.0
//! The system device backends of the `media` module: the ones that touch real hardware.
pub mod convert;
pub mod samples;

#[cfg(feature = "media-system")]
pub mod audio;
#[cfg(feature = "media-system")]
pub mod camera;
#[cfg(feature = "media-system")]
pub mod microphone;

#[cfg(feature = "media-system")]
pub use audio::SystemAudio;
#[cfg(feature = "media-system")]
pub use camera::SystemCamera;
#[cfg(feature = "media-system")]
pub use microphone::SystemMicrophone;
