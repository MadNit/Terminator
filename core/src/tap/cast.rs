//! Raw-fidelity logging in asciinema v2 (`.cast`) format.
//!
//! Deliberately an existing standard rather than a bespoke format: `.cast`
//! files replay with `asciinema play`, embed in web players, and already have
//! tooling. We get replay for free instead of inventing it.
//!
//! This tap keeps escape sequences intact, so it is the source of truth for
//! "what actually happened" -- but it is useless for grep. That is what the
//! plain-text tap is for.

use super::{Direction, Tap};
use anyhow::Result;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct CastTap {
    inner: Mutex<Inner>,
    start: Instant,
}

struct Inner {
    out: BufWriter<File>,
    /// Bytes written since the last flush; bounds data loss on a hard crash.
    since_flush: usize,
    last_flush: Instant,
    closed: bool,
}

const FLUSH_EVERY: usize = 32 * 1024;
/// Also flush on a timer. Size alone means an idle session's output can sit in
/// the buffer indefinitely -- unacceptable for a tool whose point is logging.
const FLUSH_INTERVAL: Duration = Duration::from_millis(500);

impl CastTap {
    pub fn create(path: &Path, cols: u16, rows: u16) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = BufWriter::new(File::create(path)?);

        let header = serde_json::json!({
            "version": 2,
            "width": cols,
            "height": rows,
            "timestamp": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            "env": { "TERM": "xterm-256color" },
        });
        writeln!(out, "{header}")?;
        out.flush()?;

        Ok(Self {
            inner: Mutex::new(Inner {
                out,
                since_flush: 0,
                last_flush: Instant::now(),
                closed: false,
            }),
            start: Instant::now(),
        })
    }

    fn emit(&self, code: &str, payload: &str) {
        let t = self.start.elapsed().as_secs_f64();
        let Ok(mut g) = self.inner.lock() else { return };
        if g.closed {
            return;
        }
        // serde_json handles the escaping, including invalid-UTF8 replacements.
        let line = serde_json::json!([t, code, payload]).to_string();
        if writeln!(g.out, "{line}").is_ok() {
            g.since_flush += line.len();
            if g.since_flush >= FLUSH_EVERY || g.last_flush.elapsed() >= FLUSH_INTERVAL {
                let _ = g.out.flush();
                g.since_flush = 0;
                g.last_flush = Instant::now();
            }
        }
    }
}

impl Tap for CastTap {
    fn on_data(&self, dir: Direction, data: &[u8]) {
        // Lossy is correct here: a chunk boundary can split a UTF-8 sequence,
        // and the plain-text tap keeps a properly reassembled copy anyway.
        let text = String::from_utf8_lossy(data);
        match dir {
            Direction::Output => self.emit("o", &text),
            Direction::Input => self.emit("i", &text),
        }
    }

    fn on_resize(&self, cols: u16, rows: u16) {
        self.emit("r", &format!("{cols}x{rows}"));
    }

    fn flush(&self) {
        if let Ok(mut g) = self.inner.lock() {
            if g.closed || g.since_flush == 0 {
                return;
            }
            let _ = g.out.flush();
            g.since_flush = 0;
            g.last_flush = Instant::now();
        }
    }

    fn on_close(&self) {
        if let Ok(mut g) = self.inner.lock() {
            let _ = g.out.flush();
            g.closed = true;
        }
    }
}
