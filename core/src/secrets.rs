//! Credential storage.
//!
//! Primary path is the OS keychain (Keychain / Credential Manager / libsecret).
//!
//! The fallback is not optional. On headless, minimal, or container Linux there
//! is frequently no D-Bus session and no gnome-keyring/KWallet running, so
//! `keyring` fails outright. Without a fallback the app is simply unusable
//! there -- which is exactly where an SSH client gets used most.
//!
//! The fallback is a passphrase-encrypted vault (see [`crate::vault`]), so it
//! must be unlocked before secrets can be read or written.

use crate::vault;
use anyhow::{anyhow, bail, Result};
use std::path::PathBuf;
use std::sync::Mutex;

const SERVICE: &str = "com.terminator.app";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Backend {
    /// OS-native keychain.
    Keychain,
    /// Encrypted file under the app data dir.
    File,
}

const LOCKED: &str = "secret vault is locked; unlock it with the passphrase first";

pub struct Secrets {
    fallback_dir: PathBuf,
    backend: Backend,
    /// `None` until the passphrase is supplied. Only used by `Backend::File`.
    vault: Mutex<Option<vault::Unlocked>>,
    /// Read-through cache for the keychain.
    ///
    /// Every OS keychain read can raise an authorization prompt. On macOS an
    /// unsigned or freshly rebuilt binary fails the stored ACL check, so the
    /// user is asked for their *login* password on every single access. Reading
    /// each key at most once per run turns a stream of dialogs into one.
    cache: Mutex<std::collections::HashMap<String, Option<String>>>,
}

impl Secrets {
    /// Probe the OS keychain once and remember which backend works.
    pub fn new(fallback_dir: PathBuf) -> Self {
        // Escape hatch: lets users who distrust or cannot use the platform
        // keychain opt into the vault, and makes the fallback testable on
        // machines where the keychain works fine.
        let forced = std::env::var("TERMINATOR_FORCE_FILE_SECRETS").is_ok();
        let backend = if forced {
            tracing::info!("TERMINATOR_FORCE_FILE_SECRETS set; using encrypted vault");
            Backend::File
        } else {
            match probe_keychain() {
                true => Backend::Keychain,
                false => {
                    tracing::warn!(
                        "OS keychain unavailable (no D-Bus/keyring?); using encrypted file fallback"
                    );
                    Backend::File
                }
            }
        };
        Self {
            fallback_dir,
            backend,
            vault: Mutex::new(None),
            cache: Mutex::new(std::collections::HashMap::new()),
        }
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// Force a specific backend, bypassing keychain probing.
    ///
    /// Needed for tests -- on a machine with a working keychain the file
    /// fallback would otherwise never be exercised -- and for a future
    /// "always use the vault" preference.
    pub fn with_backend(fallback_dir: PathBuf, backend: Backend) -> Self {
        Self {
            fallback_dir,
            backend,
            vault: Mutex::new(None),
            cache: Mutex::new(std::collections::HashMap::new()),
        }
    }

    pub fn set(&self, key: &str, secret: &str) -> Result<()> {
        match self.backend {
            Backend::Keychain => {
                keyring::Entry::new(SERVICE, key)?.set_password(secret)?;
                // Seed the cache so the value we just wrote does not trigger a
                // fresh authorization prompt when it is read back.
                self.cache
                    .lock()
                    .unwrap()
                    .insert(key.to_string(), Some(secret.to_string()));
                Ok(())
            }
            Backend::File => self.file_set(key, secret),
        }
    }

    pub fn get(&self, key: &str) -> Result<Option<String>> {
        match self.backend {
            Backend::Keychain => {
                if let Some(hit) = self.cache.lock().unwrap().get(key) {
                    return Ok(hit.clone());
                }
                let found = match keyring::Entry::new(SERVICE, key)?.get_password() {
                    Ok(p) => Some(p),
                    Err(keyring::Error::NoEntry) => None,
                    Err(e) => return Err(e.into()),
                };
                self.cache
                    .lock()
                    .unwrap()
                    .insert(key.to_string(), found.clone());
                Ok(found)
            }
            Backend::File => self.file_get(key),
        }
    }

    pub fn delete(&self, key: &str) -> Result<()> {
        match self.backend {
            Backend::Keychain => {
                self.cache.lock().unwrap().remove(key);
                match keyring::Entry::new(SERVICE, key)?.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                    Err(e) => Err(e.into()),
                }
            }
            Backend::File => self.file_delete(key),
        }
    }

    // ---- encrypted-file fallback ----------------------------------------

    fn vault_path(&self) -> PathBuf {
        self.fallback_dir.join("vault.bin")
    }

    /// Whether a vault already exists (i.e. the user has set a passphrase).
    pub fn vault_exists(&self) -> bool {
        self.vault_path().exists()
    }

    /// True when secrets are unusable until `unlock` is called. Always false
    /// for the keychain backend, which the OS unlocks for us.
    pub fn is_locked(&self) -> bool {
        match self.backend {
            Backend::Keychain => false,
            Backend::File => self.vault.lock().unwrap().is_none(),
        }
    }

    /// Unlock the vault, or create it if this is the first run.
    ///
    /// A wrong passphrase is reported as an error and leaves the vault locked;
    /// it never silently starts a fresh vault, which would look like every
    /// saved password had vanished.
    pub fn unlock(&self, passphrase: &str) -> Result<()> {
        if self.backend == Backend::Keychain {
            return Ok(());
        }
        let path = self.vault_path();
        let mut v = match vault::unlock(&path, passphrase)? {
            Some(v) => v,
            None => {
                tracing::info!("creating new encrypted vault at {}", path.display());
                vault::create(passphrase)?
            }
        };
        self.migrate_plaintext(&mut v)?;
        v.save(&path)?;
        *self.vault.lock().unwrap() = Some(v);
        Ok(())
    }

    pub fn lock(&self) {
        *self.vault.lock().unwrap() = None;
    }

    /// Change the passphrase. Requires the vault to be unlocked already.
    pub fn change_passphrase(&self, new_passphrase: &str) -> Result<()> {
        if self.backend == Backend::Keychain {
            bail!("passphrase does not apply to the OS keychain backend");
        }
        let mut guard = self.vault.lock().unwrap();
        let v = guard.as_mut().ok_or_else(|| anyhow!(LOCKED))?;
        vault::rekey(v, new_passphrase)?;
        v.save(&self.vault_path())
    }

    /// Adopt secrets left behind by the old plaintext fallback, then delete
    /// them. Without this the plaintext would sit on disk forever, and
    /// encrypting new secrets would be pointless theatre.
    fn migrate_plaintext(&self, v: &mut vault::Unlocked) -> Result<()> {
        let Ok(entries) = std::fs::read_dir(&self.fallback_dir) else {
            return Ok(());
        };
        let mut migrated = 0usize;
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("secret") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|x| x.to_str()) else {
                continue;
            };
            let Some(key) = unhex(stem) else { continue };
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(secret) = String::from_utf8(bytes) {
                    v.insert(&key, &secret);
                    migrated += 1;
                }
            }
            let _ = std::fs::remove_file(&path);
        }
        if migrated > 0 {
            tracing::warn!(
                "migrated {migrated} secret(s) from the old plaintext store into the encrypted vault; \
                 the plaintext files have been deleted, but assume those secrets were exposed and rotate them"
            );
        }
        Ok(())
    }

    fn file_set(&self, key: &str, secret: &str) -> Result<()> {
        let mut guard = self.vault.lock().unwrap();
        let v = guard.as_mut().ok_or_else(|| anyhow!(LOCKED))?;
        v.insert(key, secret);
        v.save(&self.vault_path())
    }

    fn file_get(&self, key: &str) -> Result<Option<String>> {
        let guard = self.vault.lock().unwrap();
        let v = guard.as_ref().ok_or_else(|| anyhow!(LOCKED))?;
        Ok(v.get(key))
    }

    fn file_delete(&self, key: &str) -> Result<()> {
        let mut guard = self.vault.lock().unwrap();
        let v = guard.as_mut().ok_or_else(|| anyhow!(LOCKED))?;
        v.remove(key);
        v.save(&self.vault_path())
    }
}

/// Reverse of the old hex filename encoding, for migration only.
fn unhex(s: &str) -> Option<String> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        out.push(u8::from_str_radix(s.get(i..i + 2)?, 16).ok()?);
    }
    String::from_utf8(out).ok()
}

/// Round-trip a throwaway value to see whether the keychain actually works.
fn probe_keychain() -> bool {
    let probe = "__terminator_probe__";
    let Ok(entry) = keyring::Entry::new(SERVICE, probe) else {
        return false;
    };
    if entry.set_password("1").is_err() {
        return false;
    }
    let ok = entry.get_password().is_ok();
    let _ = entry.delete_credential();
    ok
}

pub fn ssh_key(host: &str, user: &str) -> String {
    format!("ssh:{user}@{host}")
}

pub fn rdp_key(host: &str, user: &str) -> String {
    format!("rdp:{user}@{host}")
}
