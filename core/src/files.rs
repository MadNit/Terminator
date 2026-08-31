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

pub const IGNORED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".next",
    "dist",
    "build",
    ".cargo",
    ".vscode",
    ".idea",
    ".svn",
    ".hg",
    "__pycache__",
    ".cache",
];

/// Match information inside a single line of a file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchMatch {
    pub line_number: usize,
    pub line_content: String,
    pub match_start: usize,
    pub match_end: usize,
}

/// Search results for a single file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileSearchResult {
    pub path: String,
    pub relative_path: String,
    pub matches: Vec<SearchMatch>,
}

/// Query parameters for full-text directory search.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchOptions {
    pub query: String,
    pub case_sensitive: bool,
    pub is_regex: bool,
    pub whole_word: bool,
    pub include_pattern: Option<String>,
    pub max_results: Option<usize>,
    pub max_depth: Option<usize>,
}

pub fn matches_pattern(filename: &str, pattern: Option<&str>) -> bool {
    let Some(pattern) = pattern else { return true };
    let pattern = pattern.trim();
    if pattern.is_empty() || pattern == "*" {
        return true;
    }
    for part in pattern.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if glob_match(part, filename) {
            return true;
        }
    }
    false
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let p_lower = pattern.to_lowercase();
    let t_lower = text.to_lowercase();

    if let Some(ext) = p_lower.strip_prefix("*.") {
        return t_lower.ends_with(&format!(".{ext}")) || t_lower == ext;
    }
    if let Some(prefix) = p_lower.strip_suffix('*') {
        return t_lower.starts_with(prefix);
    }
    if p_lower.starts_with('*') && p_lower.ends_with('*') && p_lower.len() > 2 {
        let sub = &p_lower[1..p_lower.len() - 1];
        return t_lower.contains(sub);
    }
    t_lower == p_lower || t_lower.contains(&p_lower)
}

pub struct Matcher {
    regex: Option<regex::Regex>,
    plain: String,
    case_sensitive: bool,
}

impl Matcher {
    pub fn new(options: &SearchOptions) -> Result<Self> {
        let query = &options.query;
        if options.is_regex {
            let re = regex::RegexBuilder::new(query)
                .case_insensitive(!options.case_sensitive)
                .build()?;
            Ok(Self {
                regex: Some(re),
                plain: query.clone(),
                case_sensitive: options.case_sensitive,
            })
        } else if options.whole_word {
            let escaped = regex::escape(query);
            let pattern = format!(r"\b{}\b", escaped);
            let re = regex::RegexBuilder::new(&pattern)
                .case_insensitive(!options.case_sensitive)
                .build()?;
            Ok(Self {
                regex: Some(re),
                plain: query.clone(),
                case_sensitive: options.case_sensitive,
            })
        } else {
            Ok(Self {
                regex: None,
                plain: if options.case_sensitive {
                    query.clone()
                } else {
                    query.to_lowercase()
                },
                case_sensitive: options.case_sensitive,
            })
        }
    }

    pub fn find_matches_in_line(&self, line: &str, line_num: usize) -> Vec<SearchMatch> {
        let mut matches = Vec::new();
        if let Some(re) = &self.regex {
            for m in re.find_iter(line) {
                matches.push(SearchMatch {
                    line_number: line_num,
                    line_content: line.to_string(),
                    match_start: m.start(),
                    match_end: m.end(),
                });
            }
        } else if !self.plain.is_empty() {
            if self.case_sensitive {
                let mut start = 0;
                while let Some(pos) = line[start..].find(&self.plain) {
                    let match_start = start + pos;
                    let match_end = match_start + self.plain.len();
                    matches.push(SearchMatch {
                        line_number: line_num,
                        line_content: line.to_string(),
                        match_start,
                        match_end,
                    });
                    start = match_end;
                    if start >= line.len() {
                        break;
                    }
                }
            } else {
                let line_lower = line.to_lowercase();
                let mut start = 0;
                while let Some(pos) = line_lower[start..].find(&self.plain) {
                    let match_start = start + pos;
                    let match_end = match_start + self.plain.len();
                    matches.push(SearchMatch {
                        line_number: line_num,
                        line_content: line.to_string(),
                        match_start,
                        match_end,
                    });
                    start = match_end;
                    if start >= line_lower.len() {
                        break;
                    }
                }
            }
        }
        matches
    }
}

/// Search a directory tree for text matching the given query options.
pub fn search_local(root: &Path, options: &SearchOptions) -> Result<Vec<FileSearchResult>> {
    let matcher = Matcher::new(options)?;
    let mut results = Vec::new();
    let max_results = options.max_results.unwrap_or(500);
    let max_depth = options.max_depth.unwrap_or(12);
    let mut total_matches = 0;

    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut stack = vec![(canonical_root.clone(), 0)];

    while let Some((dir, depth)) = stack.pop() {
        if depth > max_depth {
            continue;
        }

        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };

        for entry in read_dir {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            let Ok(file_type) = entry.file_type() else {
                continue;
            };

            if file_type.is_dir() {
                if IGNORED_DIRS.contains(&name.as_str()) {
                    continue;
                }
                stack.push((path, depth + 1));
            } else if file_type.is_file() {
                if !matches_pattern(&name, options.include_pattern.as_deref()) {
                    continue;
                }

                let Ok(meta) = entry.metadata() else { continue };
                if meta.len() == 0 || meta.len() > 5 * 1024 * 1024 {
                    continue;
                }

                if let Ok(file_matches) = search_file_lines(&path, &matcher, max_results.saturating_sub(total_matches)) {
                    if !file_matches.is_empty() {
                        total_matches += file_matches.len();
                        let rel_path = path
                            .strip_prefix(&canonical_root)
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|_| name.clone());

                        results.push(FileSearchResult {
                            path: path.to_string_lossy().to_string(),
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

fn search_file_lines(path: &Path, matcher: &Matcher, max_file_matches: usize) -> Result<Vec<SearchMatch>> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);

    let buffer = reader.fill_buf()?;
    if buffer.contains(&0) {
        return Ok(Vec::new());
    }

    let mut matches = Vec::new();
    let mut line = String::new();
    let mut line_num = 1;

    while reader.read_line(&mut line)? > 0 {
        let trimmed_line = line.trim_end_matches(['\r', '\n']);
        let line_matches = matcher.find_matches_in_line(trimmed_line, line_num);
        for m in line_matches {
            matches.push(m);
            if matches.len() >= max_file_matches {
                return Ok(matches);
            }
        }
        line.clear();
        line_num += 1;
    }

    Ok(matches)
}

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
    /// Search full text in files starting from `root_path`.
    async fn search(&self, root_path: &str, options: &SearchOptions) -> Result<Vec<FileSearchResult>>;
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

    #[test]
    fn local_search_finds_matches_with_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(
            root.join("src").join("main.rs"),
            "fn main() {\n    println!(\"hello world\");\n    let x = 42;\n    println!(\"done\");\n}",
        )
        .unwrap();
        std::fs::write(
            root.join("README.md"),
            "# Welcome\nThis is a hello test file\n",
        )
        .unwrap();

        let options = SearchOptions {
            query: "hello".to_string(),
            case_sensitive: false,
            is_regex: false,
            whole_word: false,
            include_pattern: None,
            max_results: Some(100),
            max_depth: Some(5),
        };

        let results = search_local(root, &options).unwrap();
        assert_eq!(results.len(), 2);

        let main_rs = results.iter().find(|r| r.relative_path.contains("main.rs")).unwrap();
        assert_eq!(main_rs.matches.len(), 1);
        assert_eq!(main_rs.matches[0].line_number, 2);
        assert!(main_rs.matches[0].line_content.contains("println!(\"hello world\");"));
    }

    #[test]
    fn local_search_respects_include_pattern_and_whole_word() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("test.ts"), "const foo = 1;\nconst foobar = 2;\n").unwrap();
        std::fs::write(root.join("test.py"), "foo = 1\nfoobar = 2\n").unwrap();

        let options = SearchOptions {
            query: "foo".to_string(),
            case_sensitive: true,
            is_regex: false,
            whole_word: true,
            include_pattern: Some("*.ts".to_string()),
            max_results: Some(50),
            max_depth: Some(5),
        };

        let results = search_local(root, &options).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].relative_path, "test.ts");
        assert_eq!(results[0].matches.len(), 1);
        assert_eq!(results[0].matches[0].line_number, 1);
    }
}
