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
}

impl Default for OutputRingBuffer {
    fn default() -> Self {
        Self::new()
    }
}
