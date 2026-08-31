//! Local shell transport (ConPTY on Windows, forkpty elsewhere via portable-pty).

use super::{Transport, TransportSpec};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// portable-pty is a blocking API, so the master/child live behind std mutexes
/// and all I/O happens on dedicated threads.
pub struct PtyTransport {
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    rx: Option<mpsc::Receiver<Bytes>>,
    /// Held so the generated rc files outlive the shell that reads them.
    _shell_init: Option<crate::shell_init::ShellInit>,
}

/// Pick a sensible default shell per platform.
fn default_shell() -> String {
    #[cfg(windows)]
    {
        // Prefer PowerShell, fall back to whatever ComSpec says.
        if let Ok(p) = std::env::var("ProgramFiles") {
            let pwsh = std::path::Path::new(&p).join("PowerShell/7/pwsh.exe");
            if pwsh.exists() {
                return pwsh.to_string_lossy().into_owned();
            }
        }
        std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into())
    }
    #[cfg(not(windows))]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())
    }
}

impl PtyTransport {
    pub fn spawn(spec: &TransportSpec, cols: u16, rows: u16) -> Result<Self> {
        let (shell, cwd) = match spec {
            TransportSpec::Local { shell, cwd } => (shell.clone(), cwd.clone()),
            _ => return Err(anyhow!("PtyTransport only handles TransportSpec::Local")),
        };

        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to open pty")?;

        let program = shell.unwrap_or_else(default_shell);
        let mut cmd = CommandBuilder::new(&program);

        // Enable OSC 133 automatically. If anything about generating the
        // integration files fails, fall back to a plain login shell -- losing
        // command history is far better than failing to open a terminal.
        let mut shell_init = None;
        #[cfg(not(windows))]
        {
            let scratch = std::env::temp_dir()
                .join("terminator-shell-init")
                .join(uuid::Uuid::new_v4().to_string());
            match crate::shell_init::prepare(&program, &scratch) {
                Ok(Some(init)) => {
                    for a in &init.args {
                        cmd.arg(a);
                    }
                    for (k, v) in &init.env {
                        cmd.env(k, v);
                    }
                    shell_init = Some(init);
                }
                Ok(None) => {
                    // Unknown shell: still make it a login shell.
                    cmd.arg("-l");
                }
                Err(e) => {
                    tracing::warn!("shell integration disabled: {e}");
                    cmd.arg("-l");
                }
            }
        }

        if let Some(dir) = cwd.filter(|d| std::path::Path::new(d).is_dir()) {
            cmd.cwd(dir);
        } else if let Some(home) = dirs_home() {
            cmd.cwd(home);
        }

        // Advertise a capable terminal; xterm.js implements this level.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "Terminator");

        let child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("failed to spawn shell: {program}"))?;
        // Drop the slave handle or the master never sees EOF when the child exits.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        // Bounded so a runaway process applies backpressure instead of
        // consuming all memory. `yes` must not OOM us.
        let (tx, rx) = mpsc::channel::<Bytes>(256);

        std::thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || {
                let mut buf = vec![0u8; 65536];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if tx.blocking_send(Bytes::copy_from_slice(&buf[..n])).is_err() {
                                break; // receiver gone; session closed
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            })?;

        Ok(Self {
            master: Arc::new(Mutex::new(pair.master)),
            writer: Arc::new(Mutex::new(writer)),
            child: Arc::new(Mutex::new(child)),
            rx: Some(rx),
            _shell_init: shell_init,
        })
    }

    pub async fn exec_local(command: &str, cwd: Option<&str>) -> Result<(i32, String, String)> {
        #[cfg(windows)]
        let mut cmd = {
            let mut c = tokio::process::Command::new("cmd");
            c.args(["/C", command]);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = tokio::process::Command::new("sh");
            c.args(["-c", command]);
            c
        };
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        let output = cmd.output().await?;
        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Ok((exit_code, stdout, stderr))
    }
}

fn dirs_home() -> Option<String> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
}

#[async_trait]
impl Transport for PtyTransport {
    fn output(&mut self) -> mpsc::Receiver<Bytes> {
        self.rx.take().expect("output() called more than once")
    }

    async fn write(&self, data: Bytes) -> Result<()> {
        let writer = self.writer.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut w = writer.lock().map_err(|_| anyhow!("writer poisoned"))?;
            w.write_all(&data)?;
            w.flush()?;
            Ok(())
        })
        .await??;
        Ok(())
    }

    async fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let m = self.master.lock().map_err(|_| anyhow!("master poisoned"))?;
        m.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        let mut c = self.child.lock().map_err(|_| anyhow!("child poisoned"))?;
        let _ = c.kill();
        Ok(())
    }
}
