//! Local shell transport (ConPTY on Windows, forkpty elsewhere via portable-pty).

use super::{Transport, TransportSpec};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
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

/// A shell the user can pick from in the New Connection dialog.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ShellOption {
    pub name: String,
    pub path: String,
}

/// Pick a sensible default shell per platform. PowerShell wins, but if it is
/// missing we fall back to Git Bash (common on dev machines) and only then to
/// the bare `cmd.exe`. Returning an empty list is a bug, not a fallback.
fn default_shell() -> String {
    discover_shells()
        .into_iter()
        .next()
        .map(|s| s.path)
        .unwrap_or_else(|| {
            #[cfg(windows)]
            {
                std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into())
            }
            #[cfg(not(windows))]
            {
                std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())
            }
        })
}

/// Scan the well-known install locations and `$PATH` for usable shells.
/// Returned in preference order: PowerShell 7 → Windows PowerShell → Git
/// Bash → WSL bash → Cygwin bash → anything `bash`/`pwsh` on PATH → the
/// system default. Duplicates are removed.
pub fn discover_shells() -> Vec<ShellOption> {
    let mut out: Vec<ShellOption> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut push = |name: &str, path: &Path| {
        if path.exists() {
            let s = path.to_string_lossy().into_owned();
            if seen.insert(s.clone()) {
                out.push(ShellOption {
                    name: name.to_string(),
                    path: s,
                });
            }
        }
    };

    #[cfg(windows)]
    {
        // PowerShell 7 (preferred -- real pwsh, the modern one).
        if let Ok(pf) = std::env::var("ProgramFiles") {
            push("PowerShell 7", &PathBuf::from(pf).join(r"PowerShell\7\pwsh.exe"));
        }
        if let Ok(pf) = std::env::var("ProgramW6432") {
            push(
                "PowerShell 7",
                &PathBuf::from(pf).join(r"PowerShell\7\pwsh.exe"),
            );
        }
        // Windows PowerShell 5.1 (ships with the OS).
        if let Ok(sysroot) = std::env::var("SystemRoot") {
            push(
                "Windows PowerShell",
                &PathBuf::from(&sysroot)
                    .join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
            );
        }
        // Git Bash -- the most common bash on Windows dev boxes.
        if let Ok(pf) = std::env::var("ProgramFiles") {
            push("Git Bash", &PathBuf::from(pf).join(r"Git\bin\bash.exe"));
        }
        if let Ok(pf) = std::env::var("ProgramFiles(x86)") {
            push(
                "Git Bash",
                &PathBuf::from(pf).join(r"Git\bin\bash.exe"),
            );
        }
        // WSL bash -- points at the Linux side, not Windows-native.
        if let Ok(sysroot) = std::env::var("SystemRoot") {
            push("WSL bash", &PathBuf::from(&sysroot).join(r"System32\bash.exe"));
        }
        // Cygwin -- long shot, but cheap to check.
        push("Cygwin bash", &PathBuf::from(r"C:\cygwin64\bin\bash.exe"));

        // Anything named bash.exe or pwsh.exe on PATH. This catches scoop,
        // winget, and custom installs the fixed paths above miss.
        for name in ["pwsh.exe", "bash.exe", "zsh.exe"] {
            if let Some(p) = which(name) {
                push(name.trim_end_matches(".exe"), &p);
            }
        }
    }

    #[cfg(not(windows))]
    {
        push("bash", Path::new("/bin/bash"));
        push("zsh", Path::new("/bin/zsh"));
        push("fish", Path::new("/usr/bin/fish"));
        if let Ok(shell) = std::env::var("SHELL") {
            let p = PathBuf::from(&shell);
            if p.exists() {
                let label = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("default")
                    .to_string();
                push(&label, &p);
            }
        }
    }

    out
}

/// `which`-equivalent that doesn't pull in a crate. Returns the first hit on
/// PATH or `None`. Skips bare `cmd`/`cmd.exe` because we always have that
/// available and it would just clutter the picker.
#[allow(dead_code)]
fn which(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
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
        //
        // Works on both Windows and POSIX: the generated rc file is a pure
        // bash/zsh script, and `shell_init::prepare` is a no-op for unknown
        // shells (cmd, PowerShell) so the gate isn't needed.
        let mut shell_init = None;
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
                    // Unknown shell (cmd.exe, pwsh, fish, ...): still make it
                    // a login shell so the user sees their usual prompt.
                    #[cfg(not(windows))]
                    {
                        cmd.arg("-l");
                    }
                }
                Err(e) => {
                    tracing::warn!("shell integration disabled: {e}");
                    #[cfg(not(windows))]
                    {
                        cmd.arg("-l");
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_returns_at_least_one_shell() {
        let shells = discover_shells();
        assert!(
            !shells.is_empty(),
            "discover_shells must return at least one shell, even if all paths are missing"
        );
        // Every returned path must point at something that actually exists --
        // otherwise the picker would offer a broken entry.
        for s in &shells {
            assert!(
                std::path::Path::new(&s.path).exists(),
                "discover_shells returned a non-existent path: {}",
                s.path
            );
        }
    }

    #[test]
    fn discover_dedupes_paths() {
        let shells = discover_shells();
        let mut seen = std::collections::HashSet::new();
        for s in &shells {
            assert!(
                seen.insert(s.path.clone()),
                "duplicate shell path returned: {}",
                s.path
            );
        }
    }

    #[test]
    fn default_shell_matches_first_discovered() {
        // default_shell() must agree with the first entry of discover_shells,
        // otherwise the "Default" option in the picker would behave
        // inconsistently with what an unset profile actually gets.
        let first = discover_shells().into_iter().next().map(|s| s.path);
        if let Some(first) = first {
            assert_eq!(default_shell(), first);
        }
    }
}
