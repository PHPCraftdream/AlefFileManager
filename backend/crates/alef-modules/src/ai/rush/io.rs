// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared bounded byte accounting for Rush subprocess pipes.

/// Incremental accounting retains at most `max_line` bytes for one line and `max_total` bytes
/// overall. Callers must continue feeding/draining input after overflow to avoid pipe deadlock.
pub(crate) struct BoundedBytes {
    max_line: usize,
    max_total: usize,
    total: usize,
    pending: Vec<u8>,
    data: Vec<u8>,
    overflow: bool,
}

impl BoundedBytes {
    pub(crate) fn new(max_line: usize, max_total: usize) -> Self {
        Self {
            max_line,
            max_total,
            total: 0,
            pending: Vec::new(),
            data: Vec::new(),
            overflow: false,
        }
    }

    /// Accounts for a chunk and returns complete lines (including newline). Once a limit is
    /// exceeded no additional payload is retained, but accounting continues through EOF.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        let mut lines = Vec::new();
        for &byte in chunk {
            if self.overflow || self.total >= self.max_total || self.pending.len() >= self.max_line
            {
                self.overflow = true;
                self.pending.clear();
                continue;
            }
            self.total += 1;
            self.data.push(byte);
            self.pending.push(byte);
            if byte == b'\n' {
                lines.push(std::mem::take(&mut self.pending));
            }
        }
        lines
    }

    pub(crate) fn finish(&mut self) -> Option<Vec<u8>> {
        if self.pending.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.pending))
        }
    }

    pub(crate) fn overflowed(&self) -> bool {
        self.overflow
    }
}

/// Bounded pipe drain. The reader always consumes to EOF even after the cap is exceeded.
pub(crate) async fn drain<R: tokio::io::AsyncRead + Unpin>(mut reader: R, cap: usize) -> bool {
    use tokio::io::AsyncReadExt;
    let mut state = BoundedBytes::new(cap, cap);
    let mut chunk = vec![0; 64 * 1024];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => return state.overflowed(),
            Ok(n) => {
                state.push(&chunk[..n]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn caps_unterminated_lines_and_total_payload() {
        let mut reader = BoundedBytes::new(4096, 8192);
        let lines = reader.push(&vec![b'x'; 10 * 4096]);
        assert!(lines.is_empty());
        assert!(reader.finish().is_none());
        assert!(reader.overflowed());
    }
    #[test]
    fn emits_lines_and_flags_total_limit() {
        let mut reader = BoundedBytes::new(16, 4);
        assert_eq!(
            reader.push(b"a\nb\n"),
            vec![b"a\n".to_vec(), b"b\n".to_vec()]
        );
        reader.push(b"c\nd\n");
        assert!(reader.overflowed());
    }
}
