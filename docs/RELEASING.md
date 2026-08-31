# Releasing Terminator

How a tagged commit becomes downloadable installers, and what users actually
see when they run them.

## Cutting a release

1. Bump the version in **three** places — they must match, or the bundle
   metadata disagrees with the app:
   - `package.json` → `version`
   - `src-tauri/tauri.conf.json` → `version`
   - `src-tauri/Cargo.toml` → `[package] version`
2. Commit, then tag and push:
   ```sh
   git commit -am "release: v0.2.0"
   git tag v0.2.0
   git push origin main v0.2.0
   ```
3. The **Release** workflow builds on four runners and uploads to a **draft**
   GitHub Release.
4. Download the artifacts, sanity-check at least one per OS, write the notes,
   then publish.

The release is a draft on purpose. A broken build nobody can download yet is an
inconvenience; a broken build users have already installed is a support problem.

## What gets built

| Platform | Artifacts | Runner |
|---|---|---|
| macOS (Apple silicon) | `.dmg`, `.app.tar.gz` | `macos-latest`, `aarch64-apple-darwin` |
| macOS (Intel) | `.dmg`, `.app.tar.gz` | `macos-latest`, `x86_64-apple-darwin` |
| Linux x86_64 | `.deb`, `.rpm`, `.AppImage` | `ubuntu-22.04` |
| Windows x86_64 | `.msi`, `.exe` | `windows-latest` |

Linux builds on Ubuntu 22.04 deliberately: glibc is forward- but not
backward-compatible, so a binary built on 24.04 will not start on 22.04, while
the reverse works fine.

macOS ships as two builds rather than one universal binary. The pinned crypto
crates pull in per-architecture assembly, and two clean bundles are much easier
to diagnose than a single `lipo`'d artifact that fails on only one arch.

## Code signing

**The default builds are unsigned.** This is the biggest gap between
"I published binaries" and "people can actually install them", so it is worth
being blunt about what users hit.

### macOS

An unsigned, un-notarized `.dmg` downloaded from the internet carries a
quarantine attribute, and Gatekeeper reports:

> "Terminator" is damaged and can't be opened. You should move it to the Trash.

That message is misleading — nothing is damaged. The fix:

```sh
xattr -cr /Applications/Terminator.app
```

To remove the warning properly you need a paid Apple Developer account
($99/yr) and these repository secrets, which the release workflow already
reads:

| Secret | What it is |
|---|---|
| `APPLE_CERTIFICATE` | base64 of your "Developer ID Application" `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | password for that `.p12` |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | your Apple ID email |
| `APPLE_PASSWORD` | an **app-specific** password, not your Apple ID password |
| `APPLE_TEAM_ID` | 10-character team ID |

Export the certificate with `base64 -i certificate.p12 | pbcopy`. With those
secrets set, the same workflow signs and notarizes with no further changes.

### Windows

Unsigned installers trigger SmartScreen:

> Windows protected your PC

Users can proceed via **More info → Run anyway**. Removing the warning needs a
code-signing certificate (roughly $200–500/yr). An OV certificate still has to
accumulate download reputation before the warning disappears; an EV certificate
is trusted immediately.

### Linux

No signing gate. `.AppImage` needs `chmod +x` before it runs.

## Publishing without paying for certificates

A perfectly reasonable place to start, as long as the friction is documented
rather than hidden:

- Put the `xattr -cr` command in the release notes and the README. The workflow
  already injects it into every release body.
- Publish SHA-256 checksums so users can verify downloads.
- Point users at the build scripts — locally built binaries are never
  quarantined, so building from source sidesteps signing entirely.

## Checksums

`tauri-action` does not generate them:

```sh
gh release download v0.2.0 -D dist-release
cd dist-release && shasum -a 256 * > SHA256SUMS
gh release upload v0.2.0 SHA256SUMS
```

## Auto-updates

Not enabled. To add it later: enable the `updater` plugin, generate a keypair
with `npm run tauri signer generate`, store the private key as a repository
secret and put the public key in `tauri.conf.json`.

The updater key is entirely separate from OS code signing — it protects the
update channel, not the installer. Losing it means no existing installation can
ever update again, so back it up somewhere durable.
