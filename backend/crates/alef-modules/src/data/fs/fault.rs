// SPDX-License-Identifier: MIT OR Apache-2.0
//! What goes wrong on a disk, said in the vocabulary of `AlefError`. The words are ours: a message
//! of the system is in the language of the machine and may carry a path, and the application is
//! told neither.
use std::io;

use alef_core::{AlefError, ErrorCode};

pub(super) fn said(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::NotFound => "no such file or folder",
        ErrorCode::AlreadyExists => "it already exists",
        ErrorCode::NotADirectory => "not a folder",
        ErrorCode::IsADirectory => "it is a folder",
        ErrorCode::DirectoryNotEmpty => "the folder is not empty",
        ErrorCode::PermissionDenied => "permission denied",
        ErrorCode::Busy => "in use by another program",
        ErrorCode::Timeout => "timed out",
        ErrorCode::InvalidArgument => "invalid argument",
        _ => "the disk reported an error",
    }
}

pub(super) fn coded(code: ErrorCode) -> AlefError {
    AlefError::new(code, said(code))
}

pub(super) fn fault(error: io::Error) -> AlefError {
    // Sharing and lock violations of Windows are plain `Uncategorized` to the standard library.
    #[cfg(windows)]
    if matches!(error.raw_os_error(), Some(32 | 33)) {
        return coded(ErrorCode::Busy);
    }
    let code = match AlefError::from(error).code {
        ErrorCode::Closed | ErrorCode::NotAvailable => ErrorCode::Internal,
        code => code,
    };
    coded(code)
}

pub(super) fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}
