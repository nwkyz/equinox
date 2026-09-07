//! gettext initialization for user-visible strings.
//!
//! English is the source language (msgid), so no .mo catalog is required for
//! English to work — gettext falls back to the msgid itself. A future `po/zh_CN.po`
//! compiled to `equinox.mo` is picked up automatically.
//!
//! Locale directories are resolved with a packaging-aware fallback chain:
//! AppImage (`$APPDIR`) → user XDG data dir → system directory. A future flatpak
//! build can prepend `/app/share/locale` via build-time env injection.

use std::path::Path;

const DOMAIN: &str = "equinox";

/// Initialize gettext; safe to call multiple times (both binaries call this once).
pub fn init() {
    gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "");
    for dir in locale_candidates() {
        if Path::new(&dir).is_dir() {
            let _ = gettextrs::bindtextdomain(DOMAIN, &dir);
            break;
        }
    }
    let _ = gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8");
    let _ = gettextrs::textdomain(DOMAIN);
}

fn locale_candidates() -> Vec<String> {
    let mut dirs = Vec::new();
    if let Ok(ap) = std::env::var("APPDIR") {
        dirs.push(format!("{ap}/usr/share/locale"));
    }
    // Dev builds: build.rs writes catalogs to `<target>/<profile>/../i18n`,
    // next to the binary's parent dir (`target/debug/equinox-gui` →
    // `target/i18n`). Installed layouts don't have this, so it is skipped.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            dirs.push(
                bin_dir
                    .join("../i18n")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    if let Ok(x) = std::env::var("XDG_DATA_HOME") {
        dirs.push(format!("{x}/locale"));
    }
    if let Ok(h) = std::env::var("HOME") {
        dirs.push(format!("{h}/.local/share/locale"));
    }
    dirs.push("/usr/share/locale".into());
    dirs
}

/// Best-effort system locale as `ll-CC` (e.g. "zh-CN"), or `ll` when the
/// system language carries no region ("en"). Sources with region-sensitive
/// APIs (Bing market, Spotlight country/locale) use this for their
/// "auto (follow system)" setting.
pub fn system_locale() -> String {
    // glib merges $LC_ALL/$LC_MESSAGES/$LANG and strips encoding/suffix,
    // e.g. "zh_CN.UTF-8" → "zh_CN".
    let raw = glib::language_names()
        .first()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "en".into());
    let normalized = raw.replace('_', "-");
    let mut parts = normalized.split('-');
    let lang = parts.next().unwrap_or("en").to_ascii_lowercase();
    match parts.next() {
        Some(region) if !region.is_empty() => format!("{lang}-{}", region.to_ascii_uppercase()),
        _ => lang,
    }
}

/// Fallback `(locale, country)` for sources with region-sensitive APIs when
/// the system locale carries no explicit region: Chinese variants (`zh`,
/// `yue`, `lzh`) and Tibetan (`bo`) belong to mainland China; everything else
/// falls back to US English.
pub fn locale_fallback() -> (&'static str, &'static str) {
    let lang = system_locale();
    let lang = lang.split('-').next().unwrap_or("");
    match lang {
        "zh" | "yue" | "lzh" | "bo" => ("zh-CN", "CN"),
        _ => ("en-US", "US"),
    }
}

#[cfg(test)]
mod tests {
    use super::system_locale;

    #[test]
    fn normalizes_locale_shapes() {
        // system_locale reads glib::language_names(); in tests LANG is usually
        // unset/C, so just assert the output shape instead of exact values.
        let loc = system_locale();
        assert!(!loc.is_empty());
        if loc.contains('-') {
            let (lang, region) = loc.split_once('-').unwrap();
            assert!(lang.chars().all(|c| c.is_ascii_lowercase()));
            assert!(region.chars().all(|c| c.is_ascii_uppercase()));
        }
    }
}
