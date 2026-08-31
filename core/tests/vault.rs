//! Vault crypto tests.
//!
//! These target the properties that actually matter for a credential store:
//! a wrong passphrase must fail, tampering must be detected, nonces must never
//! repeat, and plaintext must not survive anywhere on disk.

use terminator_core::secrets::{Backend, Secrets};
use terminator_core::vault;

const PASS: &str = "correct horse battery staple";

fn vault_path(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("vault.bin")
}

#[test]
fn secrets_round_trip_through_an_encrypted_vault() {
    let dir = tempfile::tempdir().unwrap();
    let p = vault_path(dir.path());

    let mut v = vault::create(PASS).unwrap();
    v.insert("ssh:root@host", "hunter2");
    v.save(&p).unwrap();
    drop(v);

    let v2 = vault::unlock(&p, PASS)
        .unwrap()
        .expect("vault should exist");
    assert_eq!(v2.get("ssh:root@host").as_deref(), Some("hunter2"));
}

#[test]
fn the_secret_never_appears_in_plaintext_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let p = vault_path(dir.path());

    let mut v = vault::create(PASS).unwrap();
    v.insert("ssh:root@prod-db-01", "SuperSecret123");
    v.save(&p).unwrap();

    let raw = std::fs::read(&p).unwrap();
    let hay = String::from_utf8_lossy(&raw);
    assert!(
        !hay.contains("SuperSecret123"),
        "the password is sitting in the file in cleartext"
    );
    // The key name is just as sensitive: it names the host being connected to.
    assert!(
        !hay.contains("prod-db-01"),
        "the vault leaks which hosts are stored"
    );
}

#[test]
fn a_wrong_passphrase_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let p = vault_path(dir.path());

    let mut v = vault::create(PASS).unwrap();
    v.insert("k", "v");
    v.save(&p).unwrap();

    let err = vault::unlock(&p, "not the passphrase").unwrap_err();
    assert!(
        format!("{err}").contains("incorrect passphrase"),
        "unexpected error: {err}"
    );
}

#[test]
fn tampering_with_the_ciphertext_is_detected() {
    // Without AEAD authentication an attacker could flip bits in a stored
    // password. Poly1305 must catch it.
    let dir = tempfile::tempdir().unwrap();
    let p = vault_path(dir.path());

    let mut v = vault::create(PASS).unwrap();
    v.insert("k", "v");
    v.save(&p).unwrap();

    let mut raw = std::fs::read(&p).unwrap();
    let last = raw.len() - 1;
    raw[last] ^= 0x01;
    std::fs::write(&p, &raw).unwrap();

    assert!(
        vault::unlock(&p, PASS).is_err(),
        "modified ciphertext was accepted"
    );
}

#[test]
fn tampering_with_the_header_is_detected() {
    // The salt and nonce sit outside the ciphertext, so they are only protected
    // if they are bound in as AAD.
    let dir = tempfile::tempdir().unwrap();
    let p = vault_path(dir.path());

    let mut v = vault::create(PASS).unwrap();
    v.insert("k", "v");
    v.save(&p).unwrap();

    let mut raw = std::fs::read(&p).unwrap();
    raw[10] ^= 0x01; // inside the salt
    std::fs::write(&p, &raw).unwrap();

    assert!(
        vault::unlock(&p, PASS).is_err(),
        "modified header was accepted"
    );
}

#[test]
fn every_save_uses_a_fresh_nonce() {
    // Reusing an XChaCha20-Poly1305 nonce under the same key destroys
    // confidentiality outright, so this is worth asserting explicitly.
    let dir = tempfile::tempdir().unwrap();
    let p = vault_path(dir.path());
    let mut v = vault::create(PASS).unwrap();
    v.insert("k", "v");

    let mut seen = std::collections::HashSet::new();
    for _ in 0..25 {
        v.save(&p).unwrap();
        let raw = std::fs::read(&p).unwrap();
        let nonce = raw[24..48].to_vec();
        assert!(seen.insert(nonce), "nonce was reused between saves");
    }
}

#[test]
fn changing_the_passphrase_invalidates_the_old_one() {
    let dir = tempfile::tempdir().unwrap();
    let p = vault_path(dir.path());

    let mut v = vault::create(PASS).unwrap();
    v.insert("k", "v");
    v.save(&p).unwrap();

    let mut v = vault::unlock(&p, PASS).unwrap().unwrap();
    vault::rekey(&mut v, "a brand new passphrase").unwrap();
    v.save(&p).unwrap();
    drop(v);

    assert!(
        vault::unlock(&p, PASS).is_err(),
        "the old passphrase still opens the vault"
    );
    let v = vault::unlock(&p, "a brand new passphrase")
        .unwrap()
        .unwrap();
    assert_eq!(v.get("k").as_deref(), Some("v"), "contents were lost");
}

#[test]
fn an_empty_passphrase_is_refused() {
    assert!(vault::create("").is_err());
}

#[test]
fn a_foreign_file_is_not_silently_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let p = vault_path(dir.path());
    std::fs::write(&p, b"this is somebody else's file, definitely not a vault").unwrap();

    let err = vault::unlock(&p, PASS).unwrap_err();
    assert!(format!("{err}").contains("bad magic"), "got: {err}");
}

#[test]
fn missing_vault_reports_absence_rather_than_failing() {
    let dir = tempfile::tempdir().unwrap();
    assert!(vault::unlock(&vault_path(dir.path()), PASS)
        .unwrap()
        .is_none());
}

#[test]
fn file_backend_refuses_to_serve_secrets_while_locked() {
    // Failing closed matters: returning Ok(None) while locked would look like
    // "no saved password" and silently prompt the user again.
    let dir = tempfile::tempdir().unwrap();
    let s = Secrets::with_backend(dir.path().to_path_buf(), Backend::File);

    assert!(s.is_locked());
    assert!(s.set("k", "v").is_err(), "wrote a secret while locked");
    assert!(s.get("k").is_err(), "read a secret while locked");

    s.unlock(PASS).unwrap();
    assert!(!s.is_locked());
    s.set("k", "v").unwrap();
    assert_eq!(s.get("k").unwrap().as_deref(), Some("v"));

    s.lock();
    assert!(s.get("k").is_err(), "still readable after locking");
}

#[test]
fn secrets_persist_across_a_lock_unlock_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let s = Secrets::with_backend(dir.path().to_path_buf(), Backend::File);
    s.unlock(PASS).unwrap();
    s.set("ssh:me@box", "pw").unwrap();
    drop(s);

    let s2 = Secrets::with_backend(dir.path().to_path_buf(), Backend::File);
    s2.unlock(PASS).unwrap();
    assert_eq!(s2.get("ssh:me@box").unwrap().as_deref(), Some("pw"));

    s2.delete("ssh:me@box").unwrap();
    assert_eq!(s2.get("ssh:me@box").unwrap(), None);
}

#[test]
fn a_wrong_passphrase_does_not_destroy_the_existing_vault() {
    // The dangerous failure mode: treating a bad passphrase as "no vault yet"
    // and starting an empty one, wiping every saved credential.
    let dir = tempfile::tempdir().unwrap();
    let s = Secrets::with_backend(dir.path().to_path_buf(), Backend::File);
    s.unlock(PASS).unwrap();
    s.set("k", "precious").unwrap();
    s.lock();

    assert!(s.unlock("wrong").is_err());
    assert!(s.is_locked(), "a failed unlock left the vault usable");

    s.unlock(PASS).unwrap();
    assert_eq!(s.get("k").unwrap().as_deref(), Some("precious"));
}

#[test]
fn legacy_plaintext_secrets_are_migrated_and_deleted() {
    let dir = tempfile::tempdir().unwrap();
    // Recreate the old layout: hex(key) + ".secret", contents in cleartext.
    let key = "ssh:root@legacy";
    let name: String = key.bytes().map(|b| format!("{b:02x}")).collect();
    let legacy = dir.path().join(format!("{name}.secret"));
    std::fs::write(&legacy, b"old-plaintext-pw").unwrap();

    let s = Secrets::with_backend(dir.path().to_path_buf(), Backend::File);
    s.unlock(PASS).unwrap();

    assert_eq!(
        s.get(key).unwrap().as_deref(),
        Some("old-plaintext-pw"),
        "legacy secret was lost during migration"
    );
    assert!(
        !legacy.exists(),
        "plaintext file still on disk after migration"
    );
}

#[cfg(unix)]
#[test]
fn the_vault_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let s = Secrets::with_backend(dir.path().to_path_buf(), Backend::File);
    s.unlock(PASS).unwrap();
    s.set("k", "v").unwrap();

    let mode = std::fs::metadata(dir.path().join("vault.bin"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "vault is readable by other users (mode {mode:o})"
    );
}
