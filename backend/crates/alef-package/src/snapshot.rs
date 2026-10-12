// SPDX-License-Identifier: MIT OR Apache-2.0
//! Single-handle, bounded acquisition; verification consumes only the owned bytes.
use crate::{archive::MAX_ARCHIVE_SIZE, PackageError};
use std::{
    fs::OpenOptions,
    io::{self, Read},
    path::Path,
};

pub(super) fn open(path: &Path, max_bytes: u64) -> Result<Vec<u8>, PackageError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_BACKUP_SEMANTICS permits opening directories to classify
        // their handle metadata instead of reporting an unrelated access error.
        options.custom_flags(0x0200_0000);
    }
    let mut file = options.open(path)?;
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        // Windows character devices such as NUL open successfully, but cannot
        // provide file metadata (ERROR_INVALID_FUNCTION). They cannot satisfy
        // the regular-file snapshot contract; preserve every other I/O error.
        #[cfg(windows)]
        Err(error) if error.raw_os_error() == Some(1) => {
            return Err(PackageError::NotRegularFile);
        }
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() {
        return Err(PackageError::NotRegularFile);
    }
    let length = metadata.len();
    if length > MAX_ARCHIVE_SIZE {
        return Err(PackageError::ArchiveSizeLimit);
    }
    read_owned(&mut file, length, max_bytes)
}

fn read_retry(reader: &mut impl Read, output: &mut [u8]) -> io::Result<usize> {
    loop {
        match reader.read(output) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

fn read_owned(
    reader: &mut impl Read,
    initial_length: u64,
    max_bytes: u64,
) -> Result<Vec<u8>, PackageError> {
    if initial_length > max_bytes {
        return Err(PackageError::SnapshotSizeLimit(max_bytes));
    }
    let length = usize::try_from(initial_length).map_err(|_| PackageError::Allocation)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| PackageError::Allocation)?;
    bytes.resize(length, 0);
    let mut filled = 0;
    while filled < length {
        // Fixed-size requests keep short reads incremental without reading growth
        // indefinitely: at most initial_length + one probe byte is consumed.
        let end = filled + (length - filled).min(64 * 1024);
        let count = read_retry(reader, &mut bytes[filled..end])?;
        if count == 0 {
            return Err(PackageError::SnapshotLengthChanged);
        }
        filled += count;
    }
    if read_retry(reader, &mut [0; 1])? != 0 {
        return Err(if initial_length == max_bytes {
            PackageError::SnapshotSizeLimit(max_bytes)
        } else {
            PackageError::SnapshotLengthChanged
        });
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct Fragmented {
        input: Cursor<Vec<u8>>,
        chunk: usize,
        interrupted: bool,
        fail_at: Option<u64>,
    }
    impl Read for Fragmented {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if self.interrupted {
                self.interrupted = false;
                return Err(io::ErrorKind::Interrupted.into());
            }
            if self.fail_at == Some(self.input.position()) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected read failure",
                ));
            }
            let count = output.len().min(self.chunk);
            self.input.read(&mut output[..count])
        }
    }
    fn reader(bytes: &[u8], chunk: usize) -> Fragmented {
        Fragmented {
            input: Cursor::new(bytes.to_vec()),
            chunk,
            interrupted: true,
            fail_at: None,
        }
    }

    #[test]
    fn snapshot_reads_accept_one_byte_fragments_and_retry_interruptions() {
        let bytes = vec![7; 128 * 1024 + 1];
        for chunk in [1, 3, 65536] {
            let mut input = reader(&bytes, chunk);
            assert_eq!(
                read_owned(&mut input, bytes.len() as u64, bytes.len() as u64).unwrap(),
                bytes
            );
        }
    }

    #[test]
    fn snapshot_reads_accept_below_and_at_the_ceiling_but_reject_one_more_byte() {
        for length in [7, 8, 9] {
            let mut input = reader(&vec![1; length], 1);
            let result = read_owned(&mut input, length as u64, 8);
            if length <= 8 {
                assert_eq!(result.unwrap(), vec![1; length]);
            } else {
                assert!(matches!(result, Err(PackageError::SnapshotSizeLimit(8))));
                assert_eq!(input.input.position(), 0);
            }
        }
    }

    #[test]
    fn snapshot_reads_reject_growth_and_consume_only_one_extra_byte() {
        for (initial, limit) in [(3, 8), (8, 8), (0, 0), (0, u64::MAX)] {
            let mut input = reader(&[1; 32], 1);
            let error = read_owned(&mut input, initial, limit).unwrap_err();
            if initial == limit {
                assert!(matches!(error, PackageError::SnapshotSizeLimit(n) if n == limit));
            } else {
                assert!(matches!(error, PackageError::SnapshotLengthChanged));
            }
            assert_eq!(input.input.position(), initial + 1);
        }
    }

    #[test]
    fn snapshot_reads_reject_truncation_after_partial_progress() {
        let mut input = reader(b"abc", 1);
        assert!(matches!(
            read_owned(&mut input, 4, 8),
            Err(PackageError::SnapshotLengthChanged)
        ));
        assert_eq!(input.input.position(), 3);
    }

    #[test]
    fn snapshot_reads_preserve_io_errors_during_payload_and_eof_probe() {
        for fail_at in [0, 2, 4] {
            let mut input = reader(b"abcd", 1);
            input.fail_at = Some(fail_at);
            let error = read_owned(&mut input, 4, 4).unwrap_err();
            assert!(
                matches!(error, PackageError::Io(ref e) if e.kind() == io::ErrorKind::PermissionDenied && e.to_string() == "injected read failure")
            );
            assert_eq!(input.input.position(), fail_at);
        }
    }

    #[test]
    fn snapshot_reads_report_capacity_failure_before_reading() {
        let mut input = reader(b"", 1);
        assert!(matches!(
            read_owned(&mut input, u64::MAX, u64::MAX),
            Err(PackageError::Allocation)
        ));
        assert!(input.interrupted);
    }

    #[test]
    fn snapshot_reads_allow_an_empty_source_and_an_unbounded_u64_ceiling() {
        assert_eq!(read_owned(&mut Cursor::new([]), 0, 0).unwrap(), b"");
        assert_eq!(
            read_owned(&mut Cursor::new(b"abc"), 3, u64::MAX).unwrap(),
            b"abc"
        );
    }
}
