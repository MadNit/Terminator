//! File browsing and transfer.
//!
//! Two halves that deliberately share one shape: the local machine (plain
//! `std::fs`) and the remote end of a live session (SFTP). The UI drives both
//! through the same [`Listing`] / [`FileEntry`] types, so a dual-pane browser
//! does not need to care which side it is looking at.
//!
//! Remote paths are always POSIX, even when the client runs on Windows -- SFTP
//! speaks `/` regardless of what the server's native separator is. Local paths
//! use the platform separator. That asymmetry is why path joining is not
//! shared between the two.

use anyhow::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What a directory entry is, after symlinks have been resolved.
///
/// The resolved view is the useful one for a browser: a symlink pointing at a
/// directory should be enterable, exactly as it is in a shell. The `symlink`
/// flag on [`FileEntry`] preserves the fact that it *was* a link, so the UI can
/// still mark it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Dir,
    File,
    Other,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileEntry {
    pub name: String,
    /// Full path, in whichever convention the owning side uses.
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Unix epoch seconds. `None` when the platform or server withholds it.
    pub modified: Option<u64>,
    /// Dotfile. Reported rather than filtered, so the UI owns the policy.
    pub hidden: bool,
    pub symlink: bool,
}

/// One directory's worth of entries, plus enough context to navigate.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Listing {
    /// Canonical path of the directory that was listed.
    pub path: String,
    /// `None` at the root, which is what disables the "up" control.
    pub parent: Option<String>,
    pub entries: Vec<FileEntry>,
}

/// Directories first, then case-insensitive by name.
///
/// Matching the convention every file manager uses is not cosmetic: entries
/// arrive from SFTP in server order, which is effectively arbitrary, and an
/// unsorted pane is unusable on a directory of any size.
fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| {
        let a_dir = a.kind == EntryKind::Dir;
        let b_dir = b.kind == EntryKind::Dir;
        b_dir
            .cmp(&a_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

fn is_hidden(name: &str) -> bool {
    name.starts_with('.')
}

// ---------------------------------------------------------------------------
// Local
// ---------------------------------------------------------------------------

/// Where a local pane should open.
pub fn local_home() -> PathBuf {
    dirs_home().unwrap_or_else(|| PathBuf::from("/"))
}

fn dirs_home() -> Option<PathBuf> {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    std::env::var_os(key)
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
}

/// List a local directory.
///
/// Individual entries that cannot be stat'ed are kept rather than dropped: a
/// permission error on one file should not blank the whole pane, which is what
/// makes `/proc`-style directories or a locked-down `/root` behave sanely.
pub fn list_local(path: &Path) -> Result<Listing> {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut entries = Vec::new();

    for entry in std::fs::read_dir(&canonical)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        let full = entry.path();

        // symlink_metadata does not follow; metadata does. We want both: the
        // former to know it is a link, the latter to know what it points at.
        let link_meta = std::fs::symlink_metadata(&full).ok();
        let symlink = link_meta
            .as_ref()
            .is_some_and(|m| m.file_type().is_symlink());
        // A broken symlink has no target metadata; fall back to the link's own.
        let meta = std::fs::metadata(&full).ok().or(link_meta);

        let kind = match &meta {
            Some(m) if m.is_dir() => EntryKind::Dir,
            Some(m) if m.is_file() => EntryKind::File,
            Some(_) => EntryKind::Other,
            None => EntryKind::Other,
        };

        entries.push(FileEntry {
            hidden: is_hidden(&name),
            name,
            path: full.to_string_lossy().to_string(),
            kind,
            size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
            modified: meta.as_ref().and_then(mtime_secs),
            symlink,
        });
    }

    sort_entries(&mut entries);

    Ok(Listing {
        parent: canonical
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .filter(|p| !p.is_empty()),
        path: canonical.to_string_lossy().to_string(),
        entries,
    })
}

/// Read local text file up to `max_bytes`.
pub fn read_local_text(path: &Path, max_bytes: usize) -> Result<String> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    let mut handle = file.take(max_bytes as u64);
    handle.read_to_end(&mut buf)?;
    let s = String::from_utf8(buf)
        .map_err(|_| anyhow::anyhow!("File is not valid UTF-8 text or is a binary file"))?;
    Ok(s)
}

/// Write text directly to a local file.
pub fn write_local_text(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(path, content)?;
    Ok(())
}

fn mtime_secs(m: &std::fs::Metadata) -> Option<u64> {
    m.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

// ---------------------------------------------------------------------------
// Remote
// ---------------------------------------------------------------------------

/// Progress for a single transfer. `total` is 0 when the size is unknown.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Progress {
    pub transferred: u64,
    pub total: u64,
}

/// Called as a transfer advances. Invoked from the transfer task, so it must
/// not block for long.
pub type ProgressSink = Arc<dyn Fn(Progress) + Send + Sync>;

/// The remote half of a browser, backed by a live session.
///
/// Kept as a trait so the drawer is not welded to SFTP -- an RDP drive
/// redirection or a container exec backend would implement the same surface.
#[async_trait]
pub trait RemoteFs: Send + Sync {
    async fn list(&self, path: &str) -> Result<Listing>;
    /// Starting directory, resolved server-side (usually the login home).
    async fn home(&self) -> Result<String>;
    async fn mkdir(&self, path: &str) -> Result<()>;
    /// Removes a file or an (empty) directory.
    async fn remove(&self, path: &str, is_dir: bool) -> Result<()>;
    async fn rename(&self, from: &str, to: &str) -> Result<()>;
    /// Remote -> local. Returns bytes written.
    async fn download(&self, remote: &str, local: &Path, progress: ProgressSink) -> Result<u64>;
    /// Local -> remote. Returns bytes written.
    async fn upload(&self, local: &Path, remote: &str, progress: ProgressSink) -> Result<u64>;
    /// Read remote text file content up to `max_bytes`.
    async fn read_text(&self, path: &str, max_bytes: usize) -> Result<String>;
    /// Write text directly to remote file.
    async fn write_text(&self, path: &str, content: &str) -> Result<()>;
}

/// Join a POSIX path, for the remote side.
///
/// Separate from the local join because the remote convention never changes,
/// even when this code is compiled for Windows.
pub fn posix_join(base: &str, name: &str) -> String {
    if name.starts_with('/') {
        return name.to_string();
    }
    if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// Parent of a POSIX path, or `None` at the root.
pub fn posix_parent(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.rfind('/') {
        None => None,
        // A first-level entry's parent is the root itself, not "".
        Some(0) => Some("/".to_string()),
        Some(i) => Some(trimmed[..i].to_string()),
    }
}

/// Final component of a POSIX path.
pub fn posix_name(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_parent_stops_at_root() {
        assert_eq!(posix_parent("/home/user/docs"), Some("/home/user".into()));
        // The parent of a top-level directory is "/", not an empty string --
        // getting this wrong makes the "up" control navigate to nowhere.
        assert_eq!(posix_parent("/home"), Some("/".into()));
        assert_eq!(posix_parent("/"), None);
        assert_eq!(posix_parent(""), None);
    }

    #[test]
    fn posix_parent_ignores_trailing_slash() {
        assert_eq!(posix_parent("/home/user/"), Some("/home".into()));
    }

    #[test]
    fn posix_join_handles_root_and_absolute() {
        assert_eq!(posix_join("/home", "f.txt"), "/home/f.txt");
        // Root already ends in a slash; naive joining yields "//f.txt".
        assert_eq!(posix_join("/", "f.txt"), "/f.txt");
        // An absolute name replaces the base rather than nesting under it.
        assert_eq!(posix_join("/home", "/etc/hosts"), "/etc/hosts");
    }

    #[test]
    fn posix_name_returns_last_component() {
        assert_eq!(posix_name("/home/user/f.txt"), "f.txt");
        assert_eq!(posix_name("/home/user/"), "user");
    }

    #[test]
    fn local_listing_sorts_dirs_first_then_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("Zeta")).unwrap();
        std::fs::create_dir(root.join("alpha")).unwrap();
        std::fs::write(root.join("Beta.txt"), b"x").unwrap();
        std::fs::write(root.join("apple.txt"), b"xyz").unwrap();

        let listing = list_local(root).unwrap();
        let names: Vec<_> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "Zeta", "apple.txt", "Beta.txt"]);

        let apple = listing
            .entries
            .iter()
            .find(|e| e.name == "apple.txt")
            .unwrap();
        assert_eq!(apple.size, 3);
        assert_eq!(apple.kind, EntryKind::File);
    }

    #[test]
    fn local_listing_flags_hidden_without_removing_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".secret"), b"x").unwrap();

        let listing = list_local(dir.path()).unwrap();
        let hidden = listing.entries.iter().find(|e| e.name == ".secret");
        // Filtering is the UI's decision, so the entry must survive the core.
        assert!(hidden.is_some_and(|e| e.hidden));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directory_is_enterable_but_marked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("target")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("target"), dir.path().join("link")).unwrap();

        let listing = list_local(dir.path()).unwrap();
        let link = listing.entries.iter().find(|e| e.name == "link").unwrap();
        // Resolved kind, so double-clicking navigates like it does in a shell.
        assert_eq!(link.kind, EntryKind::Dir);
        assert!(link.symlink);
    }

    #[cfg(unix)]
    #[test]
    fn broken_symlink_does_not_abort_the_listing() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(dir.path().join("nope"), dir.path().join("dangling")).unwrap();
        std::fs::write(dir.path().join("real.txt"), b"x").unwrap();

        let listing = list_local(dir.path()).unwrap();
        // The whole pane must still render; a dead link is not a fatal error.
        assert_eq!(listing.entries.len(), 2);
        let dangling = listing
            .entries
            .iter()
            .find(|e| e.name == "dangling")
            .unwrap();
        assert!(dangling.symlink);
    }

    #[test]
    fn local_text_file_read_and_write_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("code.py");
        let content = "def hello():\n    print('Hello Terminator!')\n";

        write_local_text(&file_path, content).unwrap();
        let read_back = read_local_text(&file_path, 1024 * 1024).unwrap();
        assert_eq!(read_back, content);
    }
}
