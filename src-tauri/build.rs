use std::path::PathBuf;

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.is_empty() {
        let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let binaries_dir = manifest_dir.join("binaries");
        let ext = if target.contains("windows") { ".exe" } else { "" };
        let sidecar_name = format!("terminator-daemon-{}{}", target, ext);
        let sidecar_path = binaries_dir.join(&sidecar_name);

        if !sidecar_path.exists() {
            let _ = std::fs::create_dir_all(&binaries_dir);
            let target_dir = manifest_dir.parent().unwrap().join("target");
            let candidates = [
                target_dir.join("debug").join(format!("terminator-daemon{}", ext)),
                target_dir.join("release").join(format!("terminator-daemon{}", ext)),
                target_dir.join(&target).join("debug").join(format!("terminator-daemon{}", ext)),
                target_dir.join(&target).join("release").join(format!("terminator-daemon{}", ext)),
            ];

            let mut copied = false;
            for candidate in &candidates {
                if candidate.exists() {
                    if let Ok(_) = std::fs::copy(candidate, &sidecar_path) {
                        copied = true;
                        break;
                    }
                }
            }

            if !copied {
                // If the binary hasn't been compiled yet, create a stub so tauri_build succeeds.
                let _ = std::fs::write(&sidecar_path, b"");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&sidecar_path, std::fs::Permissions::from_mode(0o755));
                }
            }
        }
    }

    tauri_build::build()
}
