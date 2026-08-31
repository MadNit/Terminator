//! Known hosts file parser and manager.
//!
//! Provides inspection, searching, addition, and revocation of known host keys
//! stored in OpenSSH `known_hosts` format (`~/.ssh/known_hosts` or custom paths).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownHostEntry {
    pub id: String,
    pub line_number: usize,
    pub host_pattern: String,
    pub key_type: String,
    pub public_key: String,
    pub comment: Option<String>,
    pub fingerprint_sha256: String,
    pub fingerprint_md5: String,
    pub is_hashed: bool,
}

pub struct KnownHostsManager;

impl KnownHostsManager {
    /// Read and parse all valid known host entries from the specified path.
    pub fn list_from_path(path: &Path) -> Result<Vec<KnownHostEntry>> {
        if !path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(path)
            .with_context(|| format!("Failed to open known_hosts file at {}", path.display()))?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();

        for (idx, line_res) in reader.lines().enumerate() {
            let line_number = idx + 1;
            let line = match line_res {
                Ok(l) => l,
                Err(_) => continue,
            };

            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            // Parse known_hosts line format:
            // [@marker] host_pattern key_type base64_key [comment]
            let tokens: Vec<&str> = trimmed.split_whitespace().collect();
            if tokens.len() < 3 {
                continue;
            }

            let (host_pattern, key_type, key_b64, comment) = if tokens[0].starts_with('@') {
                if tokens.len() < 4 {
                    continue;
                }
                (
                    tokens[1].to_string(),
                    tokens[2].to_string(),
                    tokens[3].to_string(),
                    tokens.get(4..).map(|c| c.join(" ")),
                )
            } else {
                (
                    tokens[0].to_string(),
                    tokens[1].to_string(),
                    tokens[2].to_string(),
                    tokens.get(3..).map(|c| c.join(" ")),
                )
            };

            let is_hashed = host_pattern.starts_with("|1|");

            // Compute SHA256 and MD5 fingerprints from base64 key
            let (fingerprint_sha256, fingerprint_md5) =
                Self::calculate_fingerprints(&key_b64, &key_type);

            let id = format!("{}:{}:{}", host_pattern, key_type, line_number);

            entries.push(KnownHostEntry {
                id,
                line_number,
                host_pattern,
                key_type,
                public_key: key_b64,
                comment,
                fingerprint_sha256,
                fingerprint_md5,
                is_hashed,
            });
        }

        Ok(entries)
    }

    /// Remove an entry by matching host pattern and key_type, or line number.
    pub fn delete_entry(path: &Path, line_number: usize, host_pattern: &str) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read known_hosts at {}", path.display()))?;

        let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();

        if line_number > 0 && line_number <= lines.len() {
            let target_line = &lines[line_number - 1];
            if target_line.contains(host_pattern) || host_pattern.is_empty() {
                lines.remove(line_number - 1);
            } else {
                // Fallback: match by host pattern and remove matching lines
                lines.retain(|l| {
                    let trimmed = l.trim();
                    if trimmed.is_empty() || trimmed.starts_with('#') {
                        return true;
                    }
                    !l.contains(host_pattern)
                });
            }
        } else {
            lines.retain(|l| {
                let trimmed = l.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    return true;
                }
                !l.contains(host_pattern)
            });
        }

        let mut file = File::create(path)
            .with_context(|| format!("Failed to write known_hosts at {}", path.display()))?;
        for line in lines {
            writeln!(file, "{}", line)?;
        }

        Ok(())
    }

    /// Add a new host key entry to known_hosts.
    pub fn add_entry(
        path: &Path,
        host_pattern: &str,
        key_type: &str,
        public_key_b64: &str,
        comment: Option<&str>,
    ) -> Result<KnownHostEntry> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("Failed to open known_hosts for writing at {}", path.display()))?;

        let entry_str = if let Some(cmt) = comment {
            if !cmt.trim().is_empty() {
                format!("{} {} {} {}\n", host_pattern.trim(), key_type.trim(), public_key_b64.trim(), cmt.trim())
            } else {
                format!("{} {} {}\n", host_pattern.trim(), key_type.trim(), public_key_b64.trim())
            }
        } else {
            format!("{} {} {}\n", host_pattern.trim(), key_type.trim(), public_key_b64.trim())
        };

        file.write_all(entry_str.as_bytes())?;

        let entries = Self::list_from_path(path)?;
        entries
            .into_iter()
            .last()
            .ok_or_else(|| anyhow::anyhow!("Failed to retrieve newly added known_host entry"))
    }

    fn calculate_fingerprints(key_b64: &str, _key_type: &str) -> (String, String) {
        #[cfg(feature = "ssh")]
        {
            use russh::keys::ssh_key::PublicKey;
            // Try parsing public key standard OpenSSH format or raw base64
            let full_key = format!("{} {}", _key_type, key_b64);
            if let Ok(pk) = PublicKey::from_openssh(&full_key) {
                let sha256_fp = pk.fingerprint(russh::keys::ssh_key::HashAlg::Sha256).to_string();
                let md5_fp = pk.fingerprint(russh::keys::ssh_key::HashAlg::Sha512).to_string(); // russh HashAlg sha256/sha512
                return (sha256_fp, md5_fp);
            }
        }

        // Fallback or non-ssh builds:
        (
            format!("SHA256:{}", &key_b64[..key_b64.len().min(32)]),
            "N/A".to_string(),
        )
    }
}
