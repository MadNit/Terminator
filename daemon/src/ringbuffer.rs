//! Bounded ring buffer of raw bytes per session.
//!
//! The point: when the UI is closed the daemon keeps the
//! session's process alive and keeps receiving its output. When
//! the UI comes back, it needs the recent scrollback so the
//! terminal isn't blank. The buffer gives us "everything that
//! came out in the last N MB" without keeping a full history.
//!
//! This is a *byte* buffer, not an event buffer: the daemon
//! doesn't know about PTY frames or escape sequences, it just
//! stores the raw `Bytes` chunks the transport hands us. The
//! SSE handler emits them as a series of `OutputEvent::Output`
//! when a new client subscribes.

use bytes::Bytes;
use parking_lot::Mutex;
use std::collections::VecDeque;

/// Per-session output buffer. The default capacity is 1 MB,
/// which is enough to show "the last screenful or two" of a
/// 120x30 terminal at 8 KB/s output for ~2 minutes, and keeps
/// memory bounded even with many sessions.
const DEFAULT_CAPACITY_BYTES: usize = 1024 * 1024;

pub struct OutputRingBuffer {
    inner: Mutex<Inner>,
}

struct Inner {
    /// Chunks of output in arrival order. We keep a `VecDeque`
    /// of `Bytes` (not `Vec<u8>`) so that pushing a chunk is a
    /// refcount bump rather than a copy.
    chunks: VecDeque<Bytes>,
    /// Total bytes currently held across all chunks.
    total_bytes: usize,
    /// Hard upper bound. When pushing would exceed this, we
    /// drop chunks from the front until we have room.
    capacity: usize,
}

impl OutputRingBuffer {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY_BYTES)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                chunks: VecDeque::new(),
                total_bytes: 0,
                capacity,
            }),
        }
    }

    /// Append a chunk. If this would push the total over
    /// capacity, the oldest chunks are dropped from the front
    /// until the new chunk fits. Dropping a chunk is O(1) and
    /// doesn't copy.
    pub fn push(&self, chunk: Bytes) {
        let mut inner = self.inner.lock();
        let new_total = inner.total_bytes.saturating_add(chunk.len());
        if new_total > inner.capacity {
            // Drop from the front until the new chunk fits, or
            // until the buffer is empty. In the pathological
            // case where a single chunk is bigger than the
            // whole capacity, we keep just the tail of it
            // rather than dropping the new chunk entirely --
            // some scrollback is better than none.
            while inner.total_bytes + chunk.len() > inner.capacity
                && !inner.chunks.is_empty()
            {
                let dropped = inner.chunks.pop_front().unwrap();
                inner.total_bytes -= dropped.len();
            }
            if chunk.len() > inner.capacity {
                // Trim the new chunk to fit. Loses the oldest
                // `chunk.len() - capacity` bytes of this very
                // chunk, which is the best we can do.
                let start = chunk.len() - inner.capacity;
                inner
                    .chunks
                    .push_back(Bytes::copy_from_slice(&chunk[start..]));
                inner.total_bytes = inner.capacity;
                return;
            }
        }
        inner.total_bytes += chunk.len();
        inner.chunks.push_back(chunk);
    }

    /// Snapshot of the buffered output, in order. Returns owned
    /// `Bytes` (cheap, since `Bytes::clone` is a refcount bump).
    /// Used by the SSE handler to replay history on subscribe.
    pub fn snapshot(&self) -> Vec<Bytes> {
        self.inner.lock().chunks.iter().cloned().collect()
    }

    /// Number of bytes currently held.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.inner.lock().total_bytes
    }

    /// Walk the buffer and return each line that contains
    /// `needle`. Decodes each chunk as UTF-8 lossy; a match
    /// that straddles a chunk boundary is still found
    /// because we concatenate the chunks verbatim before
    /// splitting on `\n` (a chunk that already ends in
    /// `\n` does not gain an extra one).
    ///
    /// `max_results` caps the total number of matches; an
    /// `OutputRingBuffer` may have been holding output for
    /// hours, and the caller is a UI search panel.
    ///
    /// The returned `Vec` is in arrival order. Each entry
    /// includes the matched line text and the 1-based
    /// line number (within the joined output).
    pub fn search(
        &self,
        needle: &str,
        case_sensitive: bool,
        max_results: usize,
    ) -> Vec<SearchHit> {
        // Concatenate chunks verbatim. We do NOT insert a
        // separator between chunks -- a chunk that ends in
        // `\n` already provides one, and inserting another
        // would manufacture phantom empty lines that throw
        // off the line numbers. The trade-off: a chunk
        // that does NOT end in `\n` will appear as a
        // continuation of the next chunk's first line.
        // For a PTY ring buffer (always chunked on
        // natural write boundaries) this is fine.
        let joined = {
            let inner = self.inner.lock();
            let mut s = String::new();
            for chunk in inner.chunks.iter() {
                s.push_str(&String::from_utf8_lossy(chunk));
            }
            s
        };
        let needle_norm: String = if case_sensitive {
            needle.to_string()
        } else {
            needle.to_lowercase()
        };
        let mut hits = Vec::new();
        for (idx, raw_line) in joined.split('\n').enumerate() {
            if hits.len() >= max_results {
                break;
            }
            let line = if case_sensitive {
                raw_line.to_string()
            } else {
                raw_line.to_lowercase()
            };
            if line.contains(&needle_norm) {
                hits.push(SearchHit {
                    line_number: idx + 1,
                    text: raw_line.to_string(),
                });
            }
        }
        hits
    }
}

/// One matching line in `OutputRingBuffer::search`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    /// 1-based line number within the joined output.
    pub line_number: usize,
    /// The line text (decoded as UTF-8 lossy; the matched
    /// substring is the original case, since we only
    /// lowercased a copy for comparison).
    pub text: String,
}

impl Default for OutputRingBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_str(buf: &OutputRingBuffer, s: &str) {
        buf.push(bytes::Bytes::from(s.as_bytes().to_vec()));
    }

    #[test]
    fn search_finds_substring_in_lines() {
        let buf = OutputRingBuffer::new();
        push_str(&buf, "first line\n");
        push_str(&buf, "second line with NEEDLE here\n");
        push_str(&buf, "third line\n");
        let hits = buf.search("NEEDLE", false, 100);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line_number, 2);
        assert!(hits[0].text.contains("NEEDLE"));
    }

    #[test]
    fn search_is_case_insensitive_by_default() {
        let buf = OutputRingBuffer::new();
        push_str(&buf, "Foo bar baz\n");
        push_str(&buf, "BAZ qux\n");
        let hits = buf.search("baz", false, 100);
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn search_respects_case_sensitive_flag() {
        let buf = OutputRingBuffer::new();
        push_str(&buf, "Foo bar\n");
        push_str(&buf, "BAZ qux\n");
        // case-sensitive: "Foo" should NOT match "foo"
        let hits = buf.search("foo", true, 100);
        assert!(hits.is_empty());
        // case-sensitive: "BAZ" should match
        let hits = buf.search("BAZ", true, 100);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn search_caps_at_max_results() {
        let buf = OutputRingBuffer::new();
        for i in 0..100 {
            push_str(&buf, &format!("line {} with NEEDLE here\n", i));
        }
        let hits = buf.search("NEEDLE", false, 5);
        assert_eq!(hits.len(), 5);
        assert_eq!(hits[0].line_number, 1);
        assert_eq!(hits[4].line_number, 5);
    }

    #[test]
    fn search_returns_empty_when_no_match() {
        let buf = OutputRingBuffer::new();
        push_str(&buf, "hello world\n");
        assert!(buf.search("NEEDLE", false, 100).is_empty());
    }

    #[test]
    fn search_finds_match_across_chunk_boundary() {
        // The needle straddles two push() calls.
        let buf = OutputRingBuffer::new();
        push_str(&buf, "this is a NEE");
        push_str(&buf, "DLE that crosses\n");
        let hits = buf.search("NEEDLE", false, 100);
        assert_eq!(hits.len(), 1);
    }
}
