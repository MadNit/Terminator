#!/usr/bin/env bash
# Builds a release bundle (.dmg/.app on macOS, .deb/.rpm/.AppImage on Linux).
#
#   ./scripts/build.sh                     bundle for this machine
#   ./scripts/build.sh --target <triple>   cross-compile (toolchain must exist)
#   ./scripts/build.sh --no-bundle         binary only, skip installers
#
# Artifacts land in target/release/bundle/.

source "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"

PASSTHRU=("$@")

require_toolchain
ensure_node_modules

cd "$REPO_ROOT"

OS="$(os_name)"
[[ "$OS" == unsupported ]] && die "unsupported OS: $(uname -s). Use scripts/build.ps1 on Windows."

info "type-checking the frontend"
npx tsc --noEmit
ok "types clean"

info "building release bundle (this takes several minutes)"

# tauri's bundle_dmg.sh mounts a scratch volume at /Volumes/dmg.XXXXXX. If a
# previous build was interrupted -- or failed at the DMG step -- that volume
# stays mounted along with its rw.*.dmg backing file, and the next build dies
# with a bare "failed to run bundle_dmg.sh". Clearing the leftovers first turns
# a confusing hard failure into a no-op.
detach_stale_dmg_volumes() {
  [[ "$OS" == macos ]] || return 0
  for vol in /Volumes/dmg.*; do
    [[ -d "$vol" ]] || continue
    warn "detaching leftover build volume $vol"
    hdiutil detach -force "$vol" >/dev/null 2>&1 || true
  done
  rm -f "$REPO_ROOT"/target/release/bundle/macos/rw.*.dmg
}

detach_stale_dmg_volumes

# The .app we are about to overwrite may be the one the user is running, since
# that is exactly where `open target/release/bundle/macos/Terminator.app` points.
# Replacing a running bundle is a good way to get an unexplained bundling
# failure, so say so rather than letting it look like a build bug.
if [[ "$OS" == macos ]] &&
   pgrep -f 'target/release/bundle/macos/Terminator.app/Contents/MacOS' >/dev/null 2>&1; then
  warn "a Terminator.app from target/release/bundle is running; quit it if the DMG step fails"
fi

# macOS ships bash 3.2, where expanding an empty array under `set -u` is an
# "unbound variable" error. The ${a[@]+"${a[@]}"} form expands to nothing when
# the array is empty and is safe on both bash 3 and 4+.
#
# The DMG step is genuinely flaky: bundle_dmg.sh drives Finder over AppleScript
# and races with anything else touching the scratch volume (Spotlight, a cloud
# sync client, an open Finder window). When it loses that race it leaves its
# volume mounted, which then guarantees the *next* attempt fails too. Detaching
# and retrying once turns an intermittent hard failure into a slow success.
if ! npm run tauri build -- ${PASSTHRU[@]+"${PASSTHRU[@]}"}; then
  if [[ "$OS" != macos ]]; then
    die "build failed"
  fi
  warn "bundling failed; clearing the scratch volume and retrying once"
  detach_stale_dmg_volumes
  npm run tauri build -- ${PASSTHRU[@]+"${PASSTHRU[@]}"} ||
    die "build failed again after clearing the scratch volume"
fi

printf '\n'
ok "build complete"

BUNDLE_DIR="$REPO_ROOT/target/release/bundle"
if [[ -d "$BUNDLE_DIR" ]]; then
  info "artifacts:"
  find "$BUNDLE_DIR" -maxdepth 2 \
    \( -name '*.dmg' -o -name '*.app' -o -name '*.deb' \
       -o -name '*.rpm' -o -name '*.AppImage' \) \
    -exec echo '   {}' \;
fi

if [[ "$OS" == macos ]]; then
  cat <<'EOF'

  Note: this build is unsigned. macOS will refuse to open it with
  "Terminator is damaged and can't be opened" until the quarantine
  attribute is cleared:

      xattr -cr /Applications/Terminator.app

  See docs/RELEASING.md for proper signing and notarization.
EOF
fi
