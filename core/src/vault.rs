//! Passphrase-encrypted secret vault, used when no OS keychain is available.
//!
//! Argon2id derives a 32-byte key from the user's passphrase; the secret map is
//! sealed with XChaCha20-Poly1305.
//!
//! Two deliberate choices:
//!
//! * **One vault file, not one file per key.** Per-key files named after the key
//!   leak the key names through the filenamep -- `ssh:root@prod-db-01` tells an
//!   attacker your entire host inventory without decrypting anything.
//! * **No key material on disk.** The passphrase never lands anywhere; only the
//!   salt does. That is the whole point of the fallback -- if the key sat next
//!   to the ciphertext this would be obfuscation, not encryption.

use anyhow::{anyhow, bail, Context, Result};
use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, KeyInit, OsRng, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use zeroize::{Zeroize, Zeroizing};

const MAGIC: &[u8; 8] = b"TRMNTRV1";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24; // XChaCha20 uses a 192-bit nonce.
const KEY_LEN: usize = 32;
const HEADER_LEN: usize = MAGIC.len() + SALT_LEN + NONCE_LEN;

/// Argon2id cost. OWASP's recommended floor: 19 MiB, 2 passes, 1 lane.
/// Raising memory is the most effective defence against GPU cracking, but it is
/// also charged to every unlock, so this stays at the recommended floor.
const M_COST_KIB: u32 = 19 * 1024;
const T_COST: u32 = 2;
const P_COST: u32 = 1;

type Map = BTreeMap<String, String>;

/// A derived key, wiped on drop.
struct DerivedKey([u8; KEY_LEN]);

impl Drop for DerivedKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// An unlocked vault: the derived key plus the decrypted map.
pub struct Unlocked {
    key: DerivedKey,
    salt: [u8; SALT_LEN],
    map: Map,
}

impl Drop for Unlocked {
    fn drop(&mut self) {
        for v in self.map.values_mut() {
            v.zeroize();
        }
    }
}

/// Redacting `Debug`: deriving it would print every stored password the first
/// time someone writes `{:?}` in a log line.
impl std::fmt::Debug for Unlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Unlocked")
            .field("entries", &self.map.len())
            .finish_non_exhaustive()
    }
}

impl Unlocked {
    pub fn get(&self, key: &str) -> Option<String> {
        self.map.get(key).cloned()
    }

    pub fn insert(&mut self, key: &str, secret: &str) {
        if let Some(old) = self.map.get_mut(key) {
            old.zeroize();
        }
        self.map.insert(key.to_string(), secret.to_string());
    }

    pub fn remove(&mut self, key: &str) {
        if let Some(mut v) = self.map.remove(key) {
            v.zeroize();
        }
    }

    pub fn keys(&self) -> Vec<String> {
        self.map.keys().cloned().collect()
    }

    /// Seal and write the vault. A fresh nonce is generated on every save --
    /// reusing one under the same key would break XChaCha20-Poly1305 outright.
    pub fn save(&self, path: &Path) -> Result<()> {
        let plaintext = Zeroizing::new(serde_json::to_vec(&self.map)?);

        let mut nonce = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce);

        let mut header = Vec::with_capacity(HEADER_LEN);
        header.extend_from_slice(MAGIC);
        header.extend_from_slice(&self.salt);
        header.extend_from_slice(&nonce);

        let cipher = XChaCha20Poly1305::new_from_slice(&self.key.0)
            .map_err(|e| anyhow!("bad key length: {e}"))?;
        // The header is authenticated as AAD, so swapping the salt or nonce is
        // detected rather than silently producing garbage.
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: &header,
                },
            )
            .map_err(|_| anyhow!("vault encryption failed"))?;

        let mut out = header;
        out.extend_from_slice(&ciphertext);

        write_atomically(path, &out)
    }
}

/// Derive the key and decrypt the vault at `path`.
///
/// Returns `Ok(None)` when no vault exists yet (first run).
pub fn unlock(path: &Path, passphrase: &str) -> Result<Option<Unlocked>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read(path).with_context(|| format!("reading vault {}", path.display()))?;

    // Identity before integrity: a short foreign file should be reported as
    // "not ours" rather than "corrupt", which would invite the user to delete
    // somebody else's data.
    if raw.len() < MAGIC.len() || &raw[..MAGIC.len()] != MAGIC {
        bail!("not a Terminator vault (bad magic); refusing to overwrite it");
    }
    if raw.len() < HEADER_LEN + 16 {
        bail!("vault file is truncated or corrupt");
    }

    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&raw[MAGIC.len()..MAGIC.len() + SALT_LEN]);
    let nonce = &raw[MAGIC.len() + SALT_LEN..HEADER_LEN];
    let header = &raw[..HEADER_LEN];
    let ciphertext = &raw[HEADER_LEN..];

    let key = derive(passphrase, &salt)?;
    let cipher =
        XChaCha20Poly1305::new_from_slice(&key.0).map_err(|e| anyhow!("bad key length: {e}"))?;

    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: header,
            },
        )
        // An AEAD failure here is either a wrong passphrase or tampering, and
        // the two are cryptographically indistinguishable. Say so plainly
        // rather than guessing.
        .map_err(|_| anyhow!("incorrect passphrase, or the vault has been tampered with"))?;
    let plaintext = Zeroizing::new(plaintext);

    let map: Map = serde_json::from_slice(&plaintext).context("vault contents are not valid")?;

    Ok(Some(Unlocked { key, salt, map }))
}

/// Create a brand-new empty vault protected by `passphrase`.
pub fn create(passphrase: &str) -> Result<Unlocked> {
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let key = derive(passphrase, &salt)?;
    Ok(Unlocked {
        key,
        salt,
        map: Map::new(),
    })
}

/// Re-key an unlocked vault under a new passphrase.
///
/// A new salt is drawn so the old passphrase cannot be verified against the new
/// file, and the caller must `save()` for it to take effect.
pub fn rekey(v: &mut Unlocked, new_passphrase: &str) -> Result<()> {
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    v.key = derive(new_passphrase, &salt)?;
    v.salt = salt;
    Ok(())
}

fn derive(passphrase: &str, salt: &[u8]) -> Result<DerivedKey> {
    if passphrase.is_empty() {
        bail!("passphrase must not be empty");
    }
    let params = argon2::Params::new(M_COST_KIB, T_COST, P_COST, Some(KEY_LEN))
        .map_err(|e| anyhow!("bad argon2 params: {e}"))?;
    let a2 = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut out = [0u8; KEY_LEN];
    a2.hash_password_into(passphrase.as_bytes(), salt, &mut out)
        .map_err(|e| anyhow!("key derivation failed: {e}"))?;
    Ok(DerivedKey(out))
}

/// Write via a temp file + rename so an interrupted save cannot leave a
/// half-written vault. Losing this file means losing every stored password.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("vault path has no parent directory"))?;
    std::fs::create_dir_all(dir)?;

    let tmp: PathBuf = path.with_extension("tmp");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        restrict_permissions(&tmp)?;
        f.write_all(bytes)?;
        // Durability before rename, otherwise a crash can leave an empty file.
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    restrict_permissions(path)?;
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Owner-only (0600).
pub fn restrict_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path; // NTFS ACLs inherit from the per-user app data dir.
    }
    Ok(())
}
