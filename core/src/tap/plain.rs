//! Headless VT model for greppable logs and semantic command capture.
//!
//! This is the "second consumer" of the byte stream. It runs a real VT parser
//! with **no renderer attached**, purely to turn escape-code soup into clean
//! text and structured command records.
//!
//! Because it lives in the core rather than the webview, logging and search
//! keep working regardless of which UI shell is in front -- and would survive
//! swapping xterm.js out entirely.

use super::{Direction, Tap};
use anyhow::Result;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use vte::{Params, Parser, Perform};

/// One executed command, delimited by OSC 133 semantic markers.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CommandRecord {
    pub command: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}

pub type CommandSink = Box<dyn Fn(CommandRecord) + Send + Sync>;

pub struct PlainTap {
    inner: Mutex<State>,
}

struct State {
    parser: Parser,
    perf: Performer,
}

struct Performer {
    out: BufWriter<File>,
    /// Current logical line, plus the write cursor within it. Tracking the
    /// column matters: `\r` overwrites in place, so progress bars and spinners
    /// would otherwise produce thousands of junk log lines.
    line: String,
    col: usize,
    /// Full-screen apps (vim, htop, less) repaint constantly. Logging that is
    /// pure noise, so we suppress while the alternate screen is active.
    alt_screen: bool,
    /// OSC 133 state.
    phase: Phase,
    cmd_buf: String,
    /// Set once `133;E` reported the command line verbatim.
    cmd_explicit: bool,
    cmd_start: Option<Instant>,
    sink: Option<CommandSink>,
    last_flush: Instant,
    closed: bool,
}

/// Flush on a timer so the log is useful to `tail -f` and survives a crash.
const FLUSH_INTERVAL: Duration = Duration::from_millis(500);

#[derive(PartialEq, Clone, Copy)]
enum Phase {
    Idle,
    /// Between OSC 133;B and ;C -- the shell is echoing the command line.
    Command,
    /// After OSC 133;C -- command output.
    Output,
}

impl PlainTap {
    pub fn create(path: &Path) -> Result<Self> {
        Self::with_sink(path, None)
    }

    pub fn with_sink(path: &Path, sink: Option<CommandSink>) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let out = BufWriter::new(File::create(path)?);
        Ok(Self {
            inner: Mutex::new(State {
                parser: Parser::new(),
                perf: Performer {
                    out,
                    line: String::new(),
                    col: 0,
                    alt_screen: false,
                    phase: Phase::Idle,
                    cmd_buf: String::new(),
                    cmd_explicit: false,
                    cmd_start: None,
                    sink,
                    last_flush: Instant::now(),
                    closed: false,
                },
            }),
        })
    }
}

impl Performer {
    fn flush_line(&mut self) {
        if !self.alt_screen && !self.line.trim_end().is_empty() {
            let _ = writeln!(self.out, "{}", self.line.trim_end());
            if self.last_flush.elapsed() >= FLUSH_INTERVAL {
                let _ = self.out.flush();
                self.last_flush = Instant::now();
            }
        }
        self.line.clear();
        self.col = 0;
    }
}

impl Perform for Performer {
    fn print(&mut self, c: char) {
        if self.phase == Phase::Command && !self.cmd_explicit {
            self.cmd_buf.push(c);
        }
        if self.alt_screen {
            return;
        }
        // Overwrite semantics at the cursor rather than blind append.
        let byte_idx = self
            .line
            .char_indices()
            .nth(self.col)
            .map(|(i, _)| i)
            .unwrap_or(self.line.len());
        if byte_idx < self.line.len() {
            let mut it = self.line[byte_idx..].chars();
            let old = it.next().map(|c| c.len_utf8()).unwrap_or(0);
            self.line
                .replace_range(byte_idx..byte_idx + old, &c.to_string());
        } else {
            self.line.push(c);
        }
        self.col += 1;
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.flush_line(),
            b'\r' => self.col = 0,
            b'\t' => {
                let next = (self.col / 8 + 1) * 8;
                while self.col < next {
                    self.print(' ');
                }
            }
            0x08 => self.col = self.col.saturating_sub(1),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        // Track alternate-screen enter/exit: CSI ? 1049 h / l (also 47, 1047).
        if intermediates.first() == Some(&b'?') && matches!(action, 'h' | 'l') {
            for p in params.iter() {
                if matches!(p.first(), Some(1049) | Some(1047) | Some(47)) {
                    if action == 'h' {
                        self.flush_line();
                        self.alt_screen = true;
                    } else {
                        self.alt_screen = false;
                        self.line.clear();
                        self.col = 0;
                    }
                }
            }
        }
        // Erase-in-line (CSI K) with no param clears from cursor to EOL.
        if action == 'K' && !self.alt_screen {
            let n = params
                .iter()
                .next()
                .and_then(|p| p.first().copied())
                .unwrap_or(0);
            if n == 0 {
                let idx = self
                    .line
                    .char_indices()
                    .nth(self.col)
                    .map(|(i, _)| i)
                    .unwrap_or(self.line.len());
                self.line.truncate(idx);
            }
        }
    }

    /// OSC 133 is the shell-integration protocol that makes everything else
    /// possible: it tells us exactly where each prompt, command and output
    /// block begins and ends, and what the exit code was.
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let Some(&first) = params.first() else { return };
        if first != b"133" {
            return;
        }
        let Some(&kind) = params.get(1) else { return };
        match kind.first() {
            Some(b'A') => {
                // Prompt start.
                self.flush_line();
                self.phase = Phase::Idle;
            }
            Some(b'B') => {
                // Command entry begins; start capturing the echoed command.
                // Only used for third-party integrations that lack `E`.
                if !self.cmd_explicit {
                    self.cmd_buf.clear();
                }
                self.phase = Phase::Command;
            }
            Some(b'E') => {
                // The shell told us the command line verbatim. Authoritative:
                // never let echoed screen text override it.
                //
                // OSC params split on ';', so a command containing one arrives
                // pre-split -- rejoin everything past the marker.
                if params.len() > 2 {
                    let joined = params[2..]
                        .iter()
                        .map(|p| String::from_utf8_lossy(p))
                        .collect::<Vec<_>>()
                        .join(";");
                    self.cmd_buf = joined;
                    self.cmd_explicit = true;
                }
            }
            Some(b'C') => {
                self.phase = Phase::Output;
                self.cmd_start = Some(Instant::now());
            }
            Some(b'D') => {
                // Command finished; params[2] carries the exit code.
                let exit = params
                    .get(2)
                    .and_then(|p| std::str::from_utf8(p).ok())
                    .and_then(|s| s.trim().parse::<i32>().ok());
                let cmd = self.cmd_buf.trim().to_string();
                let dur = self
                    .cmd_start
                    .map(|s| s.elapsed())
                    .unwrap_or(Duration::ZERO);
                if !cmd.is_empty() {
                    if let Some(sink) = &self.sink {
                        sink(CommandRecord {
                            command: cmd,
                            exit_code: exit,
                            duration_ms: dur.as_millis() as u64,
                        });
                    }
                }
                self.cmd_buf.clear();
                self.cmd_explicit = false;
                self.cmd_start = None;
                self.phase = Phase::Idle;
            }
            _ => {}
        }
    }
}

impl Tap for PlainTap {
    fn on_data(&self, dir: Direction, data: &[u8]) {
        if dir != Direction::Output {
            return; // input is echoed back by the remote; logging it duplicates
        }
        let Ok(mut g) = self.inner.lock() else { return };
        if g.perf.closed {
            return;
        }
        let State { parser, perf } = &mut *g;
        parser.advance(perf, data);
    }

    fn flush(&self) {
        if let Ok(mut g) = self.inner.lock() {
            if !g.perf.closed {
                let _ = g.perf.out.flush();
            }
        }
    }

    fn on_close(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.perf.flush_line();
            let _ = g.perf.out.flush();
            g.perf.closed = true;
        }
    }
}
