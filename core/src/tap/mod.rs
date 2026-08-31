//! The byte tap: one stream, many consumers.
//!
//! Every byte that flows through a session is offered to each registered tap.
//! Rendering, logging, search indexing and triggers are all just taps, so none
//! of them depend on the UI being alive, focused, or even present.
//!
//! Three tap points, deliberately, because each captures something the others
//! lose:
//!   * raw bytes      -> perfect fidelity, replayable, but full of escape codes
//!   * headless screen -> clean greppable text, but loses timing/colour
//!   * semantic (OSC 133) -> per-command records with exit codes
//!
//! Taps must never block the session loop: they are called inline, so anything
//! slow (disk, network) has to buffer internally.

pub mod cast;
pub mod plain;

use std::sync::Arc;

/// Direction of a chunk of data relative to the local user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Remote -> us (program output).
    Output,
    /// Us -> remote (keystrokes, paste).
    Input,
}

/// A consumer of a session's byte stream.
///
/// All methods have default no-op impls so a tap only implements what it needs.
#[allow(unused_variables)]
pub trait Tap: Send + Sync {
    fn on_data(&self, dir: Direction, data: &[u8]) {}
    fn on_resize(&self, cols: u16, rows: u16) {}
    /// Periodic tick so buffered writers reach disk on idle sessions.
    /// Without this, a burst of output followed by silence would sit in a
    /// buffer indefinitely -- logs must be tailable in real time.
    fn flush(&self) {}
    /// Session ended; flush and close any handles.
    fn on_close(&self) {}
}

/// Fan-out to every registered tap.
#[derive(Clone, Default)]
pub struct TapSet {
    taps: Vec<Arc<dyn Tap>>,
}

impl TapSet {
    pub fn new() -> Self {
        Self { taps: Vec::new() }
    }

    pub fn push(&mut self, tap: Arc<dyn Tap>) {
        self.taps.push(tap);
    }

    pub fn is_empty(&self) -> bool {
        self.taps.is_empty()
    }

    pub fn on_data(&self, dir: Direction, data: &[u8]) {
        for t in &self.taps {
            t.on_data(dir, data);
        }
    }

    pub fn on_resize(&self, cols: u16, rows: u16) {
        for t in &self.taps {
            t.on_resize(cols, rows);
        }
    }

    pub fn on_close(&self) {
        for t in &self.taps {
            t.on_close();
        }
    }

    pub fn flush(&self) {
        for t in &self.taps {
            t.flush();
        }
    }
}
