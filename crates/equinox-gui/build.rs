//! Compile any translation catalogs in `po/` into `target/i18n/<locale>/LC_MESSAGES/equinox.mo`.
//!
//! English is the source language (msgid), so no catalog is required for
//! English. When a `po/zh_CN.po` is added later, this script picks it up
//! automatically and `data/install.sh` installs the .mo files into
//! `$PREFIX/share/locale`.
//!
//! Uses the system `msgfmt` (gettext-tools); when it is missing, translation
//! compilation is skipped with a warning — the app still runs in English.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Build scripts run with the *package* dir as cwd and no CARGO_TARGET_DIR,
    // so resolve everything from CARGO_MANIFEST_DIR (crates/equinox-gui):
    // the catalog sources live in the workspace-root `po/`.
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let ws_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("package must live in <ws>/crates/equinox-gui");
    let po_dir = ws_root.join("po");
    // CARGO_TARGET_DIR is only present when the user sets it explicitly.
    let out = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| ws_root.join("target"));
    // Track the directory itself, so newly added catalogs are picked up.
    println!("cargo:rerun-if-changed={}", po_dir.display());

    let Ok(entries) = std::fs::read_dir(&po_dir) else {
        // No po/ directory yet — nothing to compile.
        return;
    };

    let msgfmt = Command::new("msgfmt").arg("--version").output().is_ok();
    if !msgfmt {
        println!(
            "cargo:warning=msgfmt not found; skipping po/*.po compilation (app will show English)"
        );
        return;
    }

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("po") {
            continue;
        }
        let locale = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("po file name must be valid UTF-8");
        let mo = out.join(format!("i18n/{locale}/LC_MESSAGES/equinox.mo"));
        if let Some(parent) = mo.parent() {
            std::fs::create_dir_all(parent).expect("failed to create i18n output dir");
        }
        println!("cargo:rerun-if-changed={}", path.display());
        let status = Command::new("msgfmt")
            .args(["--check", "-o"])
            .arg(&mo)
            .arg(&path)
            .status()
            .expect("failed to run msgfmt");
        if !status.success() {
            panic!("msgfmt failed for {}", path.display());
        }
    }
}
