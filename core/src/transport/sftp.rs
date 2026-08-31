//! SFTP-backed [`RemoteFs`], running over a live SSH session.
//!
//! Transfers stream in chunks rather than buffering whole files: the drawer is
//! expected to move disk images and log archives, and reading those into memory
//! first would be an easy way to kill the app on a large file.

use crate::files::{
    matches_pattern, posix_join, posix_parent, EntryKind, FileEntry, FileSearchResult,
    Listing, Matcher, Progress, ProgressSink, RemoteFs, SearchOptions, IGNORED_DIRS,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use russh_sftp::client::SftpSession;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Transfer chunk. 32 KiB is a common SFTP payload ceiling; larger reads get
/// split by the server anyway, so a bigger buffer buys nothing.
const CHUNK: usize = 32 * 1024;

/// How often progress is reported, as a byte interval. Emitting per chunk
/// would flood the IPC bridge on a fast link -- a 1 GiB file at 32 KiB per
/// chunk is 32k events.
const PROGRESS_INTERVAL: u64 = 512 * 1024;

pub struct SftpFs {
    sftp: Arc<SftpSession>,
}

impl SftpFs {
    pub fn new(sftp: Arc<SftpSession>) -> Self {
        Self { sftp }
    }
}

#[async_trait]
impl RemoteFs for SftpFs {
    async fn list(&self, path: &str) -> Result<Listing> {
        // Resolve first so ".", "~" style relatives and symlinked directories
        // produce a stable, canonical path for the breadcrumb.
        let canonical = self
            .sftp
            .canonicalize(path)
            .await
            .unwrap_or_else(|_| path.to_string());

        let dir = self
            .sftp
            .read_dir(canonical.clone())
            .await
            .with_context(|| format!("cannot list {canonical}"))?;

        let mut entries = Vec::new();
        for item in dir {
            let name = item.file_name();
            // The server includes these; a browser with its own "up" control
            // does not want them as rows.
            if name == "." || name == ".." {
                continue;
            }
            let meta = item.metadata();
            let file_type = item.file_type();

            // A symlink's own attributes describe the link, not the target, so
            // a symlinked directory would otherwise show up as a plain file and
            // refuse to open. Re-stat through the link to get the real type.
            let (kind, size) = if file_type.is_symlink() {
                match self.sftp.metadata(posix_join(&canonical, &name)).await {
                    Ok(target) => (kind_of(target.is_dir(), target.is_regular()), target.size),
                    // Broken link: keep the row, just not navigable.
                    Err(_) => (EntryKind::Other, meta.size),
                }
            } else {
                (kind_of(file_type.is_dir(), file_type.is_file()), meta.size)
            };

            entries.push(FileEntry {
                path: posix_join(&canonical, &name),
                hidden: name.starts_with('.'),
                name,
                kind,
                size: size.unwrap_or(0),
                modified: meta.mtime.map(u64::from),
                symlink: file_type.is_symlink(),
            });
        }

        entries.sort_by(|a, b| {
            let a_dir = a.kind == EntryKind::Dir;
            let b_dir = b.kind == EntryKind::Dir;
            b_dir
                .cmp(&a_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        Ok(Listing {
            parent: posix_parent(&canonical),
            path: canonical,
            entries,
        })
    }

    async fn home(&self) -> Result<String> {
        // Canonicalizing "." yields the SFTP start directory, which is the
        // login home on every server worth supporting.
        self.sftp
            .canonicalize(".")
            .await
            .context("cannot resolve the remote home directory")
    }

    async fn mkdir(&self, path: &str) -> Result<()> {
        self.sftp
            .create_dir(path)
            .await
            .with_context(|| format!("cannot create {path}"))
    }

    async fn remove(&self, path: &str, is_dir: bool) -> Result<()> {
        if is_dir {
            self.sftp.remove_dir(path).await
        } else {
            self.sftp.remove_file(path).await
        }
        .with_context(|| format!("cannot remove {path}"))
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.sftp
            .rename(from, to)
            .await
            .with_context(|| format!("cannot rename {from} to {to}"))
    }

    async fn download(&self, remote: &str, local: &Path, progress: ProgressSink) -> Result<u64> {
        let total = self
            .sftp
            .metadata(remote.to_string())
            .await
            .ok()
            .and_then(|m| m.size)
            .unwrap_or(0);

        let mut src = self
            .sftp
            .open(remote.to_string())
            .await
            .with_context(|| format!("cannot open {remote}"))?;

        if let Some(parent) = local.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let mut dst = tokio::fs::File::create(local)
            .await
            .with_context(|| format!("cannot write {}", local.display()))?;

        let moved = copy_with_progress(&mut src, &mut dst, total, &progress).await?;
        // Without an explicit flush the tail of the file can still be sitting
        // in the buffer when we report success.
        dst.flush().await?;
        Ok(moved)
    }

    async fn upload(&self, local: &Path, remote: &str, progress: ProgressSink) -> Result<u64> {
        let total = tokio::fs::metadata(local)
            .await
            .map(|m| m.len())
            .unwrap_or(0);

        let mut src = tokio::fs::File::open(local)
            .await
            .with_context(|| format!("cannot read {}", local.display()))?;
        let mut dst = self
            .sftp
            .create(remote.to_string())
            .await
            .with_context(|| format!("cannot create {remote}"))?;

        let moved = copy_with_progress(&mut src, &mut dst, total, &progress).await?;
        // SFTP writes are pipelined; shutdown waits for the server to ack them.
        // Skipping it silently truncates the tail of every upload.
        dst.shutdown().await?;
        Ok(moved)
    }

    async fn read_text(&self, path: &str, max_bytes: usize) -> Result<String> {
        let mut src = self
            .sftp
            .open(path.to_string())
            .await
            .with_context(|| format!("cannot open {path}"))?;

        let mut buf = Vec::new();
        let mut limited = (&mut src).take(max_bytes as u64);
        limited.read_to_end(&mut buf).await?;

        let s = String::from_utf8(buf)
            .map_err(|_| anyhow::anyhow!("File is not valid UTF-8 text or is a binary file"))?;
        Ok(s)
    }

    async fn write_text(&self, path: &str, content: &str) -> Result<()> {
        let mut dst = self
            .sftp
            .create(path.to_string())
            .await
            .with_context(|| format!("cannot create {path}"))?;

        dst.write_all(content.as_bytes()).await?;
        dst.flush().await?;
        dst.shutdown().await?;
        Ok(())
    }

    async fn search(&self, root_path: &str, options: &SearchOptions) -> Result<Vec<FileSearchResult>> {
        let matcher = Matcher::new(options)?;
        let canonical = self
            .sftp
            .canonicalize(root_path)
            .await
            .unwrap_or_else(|_| root_path.to_string());

        let max_results = options.max_results.unwrap_or(300);
        let max_depth = options.max_depth.unwrap_or(8);
        let mut total_matches = 0;
        let mut results = Vec::new();

        let mut stack = vec![(canonical.clone(), 0)];

        while let Some((dir_path, depth)) = stack.pop() {
            if depth > max_depth {
                continue;
            }

            let Ok(dir) = self.sftp.read_dir(dir_path.clone()).await else {
                continue;
            };

            for item in dir {
                let name = item.file_name();
                if name == "." || name == ".." {
                    continue;
                }

                let full_path = posix_join(&dir_path, &name);
                let file_type = item.file_type();

                if file_type.is_dir() {
                    if IGNORED_DIRS.contains(&name.as_str()) {
                        continue;
                    }
                    stack.push((full_path, depth + 1));
                } else if file_type.is_file() {
                    if !matches_pattern(&name, options.include_pattern.as_deref()) {
                        continue;
                    }

                    let meta = item.metadata();
                    let size = meta.size.unwrap_or(0);
                    if size == 0 || size > 2 * 1024 * 1024 {
                        continue;
                    }

                    if let Ok(content) = self.read_text(&full_path, 2 * 1024 * 1024).await {
                        let mut file_matches = Vec::new();
                        for (idx, line) in content.lines().enumerate() {
                            let line_num = idx + 1;
                            let found = matcher.find_matches_in_line(line, line_num);
                            for m in found {
                                file_matches.push(m);
                                if file_matches.len() + total_matches >= max_results {
                                    break;
                                }
                            }
                            if file_matches.len() + total_matches >= max_results {
                                break;
                            }
                        }

                        if !file_matches.is_empty() {
                            total_matches += file_matches.len();
                            let rel_path = if full_path.starts_with(&canonical) {
                                full_path[canonical.len()..]
                                    .trim_start_matches('/')
                                    .to_string()
                            } else {
                                name.clone()
                            };

                            results.push(FileSearchResult {
                                path: full_path,
                                relative_path: if rel_path.is_empty() { name } else { rel_path },
                                matches: file_matches,
                            });

                            if total_matches >= max_results {
                                return Ok(results);
                            }
                        }
                    }
                }
            }
        }

        Ok(results)
    }
}

fn kind_of(is_dir: bool, is_file: bool) -> EntryKind {
    if is_dir {
        EntryKind::Dir
    } else if is_file {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

/// Stream `src` into `dst`, reporting progress at coarse intervals.
///
/// Always emits a final event with the true byte count so the UI can settle on
/// 100% even when the advertised size was wrong or absent.
async fn copy_with_progress<R, W>(
    src: &mut R,
    dst: &mut W,
    total: u64,
    progress: &ProgressSink,
) -> Result<u64>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; CHUNK];
    let mut moved: u64 = 0;
    let mut last_report: u64 = 0;

    loop {
        let n = src.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n]).await?;
        moved += n as u64;

        if moved - last_report >= PROGRESS_INTERVAL {
            last_report = moved;
            progress(Progress {
                transferred: moved,
                total,
            });
        }
    }

    progress(Progress {
        transferred: moved,
        total: total.max(moved),
    });
    Ok(moved)
}
