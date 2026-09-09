//! Global config, stored as JSON at `~/.config/equinox/config.json`.
//! The daemon and GUI each hold a [`Config`] object, but **every read/write
//! goes straight to disk** — both processes share the same file, so any write
//! by one is immediately visible to the other, and stale in-memory snapshots
//! can never overwrite each other.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

use crate::source::SourceSettings;

/// Default refresh interval for scheduled updates (seconds).
pub const DEFAULT_UPDATE_INTERVAL: i64 = 3600;
/// Interval bounds: at most one update per 5 minutes (API courtesy),
/// at least one per week.
pub const MIN_UPDATE_INTERVAL: i64 = 300;
pub const MAX_UPDATE_INTERVAL: i64 = 7 * 24 * 3600;
/// Default per-source image cap (0 = unlimited).
pub const DEFAULT_MAX_IMAGES: i64 = 100;
/// Wallpaper-backend configuration values (see [`Config::backend`]).
pub const BACKEND_AUTO: &str = "auto";
pub const BACKEND_GNOME: &str = "gnome";
pub const BACKEND_KDE: &str = "kde";
pub const BACKEND_XFCE: &str = "xfce";

/// Config file path: `~/.config/equinox/config.json`.
fn config_path() -> PathBuf {
    glib::user_config_dir().join("equinox").join("config.json")
}

#[derive(Clone)]
pub struct Config {
    path: PathBuf,
}

impl Config {
    /// Open the config (no disk write; the file is created on first save).
    pub fn new() -> Result<Self> {
        Ok(Self {
            path: config_path(),
        })
    }

    /// Read the whole config from disk; missing returns empty, corrupt gets
    /// backed up and returns empty (keeping the app usable).
    fn load_map(&self) -> Map<String, Value> {
        match load(&self.path) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("failed to read config ({e}), backed up to config.json.bak");
                let _ = std::fs::rename(&self.path, self.path.with_extension("json.bak"));
                Map::new()
            }
        }
    }

    /// Top-level string key; missing returns empty string.
    fn get_str(&self, key: &str) -> String {
        self.load_map()
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned()
    }

    /// Unified write entry: re-read disk → apply change → atomic save
    /// (temp file + rename).
    fn mutate(&self, f: impl FnOnce(&mut Map<String, Value>)) {
        let mut map = self.load_map();
        f(&mut map);
        if let Err(e) = save(&self.path, &map) {
            log::warn!("failed to write config: {e}");
        }
    }

    /// Current wallpaper source id; empty string means none selected.
    pub fn wallpaper_source(&self) -> String {
        self.get_str("wallpaper-source")
    }

    pub fn set_wallpaper_source(&self, source: &str) {
        self.set_str("wallpaper-source", source);
    }

    fn set_str(&self, key: &str, value: &str) {
        self.mutate(|map| {
            map.insert(key.to_owned(), json!(value));
        });
    }

    /// The user's original wallpaper (a local path), captured right before
    /// Equinox's first apply so the "None" source can restore it. Empty when
    /// nothing has been applied yet or the original could not be read.
    pub fn original_wallpaper(&self) -> String {
        self.get_str("original-wallpaper")
    }

    pub fn set_original_wallpaper(&self, path: &str) {
        self.set_str("original-wallpaper", path);
    }

    /// Wallpaper mode: "latest" | "random" (default "latest", old-data compatible).
    pub fn wallpaper_mode(&self) -> String {
        let m = self.get_str("wallpaper-mode");
        if m.is_empty() {
            "latest".to_owned()
        } else {
            m
        }
    }

    pub fn set_wallpaper_mode(&self, mode: &str) {
        self.set_str("wallpaper-mode", mode);
    }

    /// A source's wallpaper mode ("latest"/"random"/"manual"). The source's
    /// own setting wins; falls back to the global key (old-data compatible).
    pub fn source_mode(&self, source: &str) -> String {
        let m = self.settings_for(source).get_str("mode", "");
        if m.is_empty() {
            self.wallpaper_mode()
        } else {
            m
        }
    }

    /// Set a source's wallpaper mode (mode is remembered per source and
    /// restored when switching back to it).
    pub fn set_source_mode(&self, source: &str, mode: &str) {
        self.set_source_setting(source, "mode", json!(mode));
    }

    /// Whether scheduled updates are enabled for a source.
    pub fn update_enabled(&self, source: &str) -> bool {
        self.settings_for(source).get_bool("update-enabled", false)
    }

    pub fn set_update_enabled(&self, source: &str, enabled: bool) {
        self.set_source_setting(source, "update-enabled", json!(enabled));
    }

    /// A source's refresh interval in seconds (scheduled updates fire when
    /// the elapsed time since the last update reaches this).
    pub fn update_interval(&self, source: &str) -> i64 {
        clamp_interval(
            self.settings_for(source)
                .get_i64("update-interval", DEFAULT_UPDATE_INTERVAL),
        )
    }

    pub fn set_update_interval(&self, source: &str, secs: i64) {
        self.set_source_setting(source, "update-interval", json!(clamp_interval(secs)));
    }

    /// Unix timestamp of a source's last update attempt (persisted, so the
    /// schedule survives restarts; after the machine was off/asleep past an
    /// interval, the next tick fires immediately as catch-up).
    pub fn last_updated_at(&self, source: &str) -> i64 {
        self.settings_for(source)
            .get_i64("last-updated-at", 0)
    }

    pub fn set_last_updated_at(&self, source: &str, ts: i64) {
        self.set_source_setting(source, "last-updated-at", json!(ts));
    }

    /// All source settings: source_id -> {key: value}.
    pub fn source_settings_map(&self) -> Map<String, Value> {
        self.load_map()
            .get("source-settings")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default()
    }

    /// A source's settings (only configured keys).
    pub fn settings_for(&self, source_id: &str) -> SourceSettings {
        SourceSettings(
            self.source_settings_map()
                .get(source_id)
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default(),
        )
    }

    /// Update a single setting key of a source and persist.
    pub fn set_source_setting(&self, source_id: &str, key: &str, value: Value) {
        self.mutate(|map| {
            let mut settings = map
                .get("source-settings")
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();
            let source = settings
                .entry(source_id.to_owned())
                .or_insert_with(|| json!({}));
            if let Some(obj) = source.as_object_mut() {
                obj.insert(key.to_owned(), value);
            }
            map.insert("source-settings".to_owned(), json!(settings));
        });
    }

    /// Whether the user wants the background (daemon/supervisor) to start
    /// automatically at login (XDG autostart; maintained by the GUI/OOBE).
    pub fn autostart(&self) -> bool {
        self.load_map()
            .get("autostart")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    pub fn set_autostart(&self, enabled: bool) {
        self.mutate(|map| {
            map.insert("autostart".to_owned(), json!(enabled));
        });
    }

    /// Background runtime mode: "supervisor" (default, standalone
    /// supervisor+daemon, no systemd) or "systemd" (daemon wrapped in a
    /// systemd user service). Picked in the first-run OOBE / Preferences.
    pub fn run_mode(&self) -> String {
        let m = self.get_str("run-mode");
        if m.is_empty() {
            "supervisor".to_owned()
        } else {
            m
        }
    }

    pub fn set_run_mode(&self, mode: &str) {
        self.set_str("run-mode", mode);
    }

    /// GUI display language: "auto" (follow system, default) or a gettext
    /// locale name ("en", "zh_CN", "ja", "ru", "bo", …). Read once at GUI
    /// startup — changing it takes effect after restarting the GUI.
    pub fn language(&self) -> String {
        let l = self.get_str("language");
        if l.is_empty() {
            "auto".to_owned()
        } else {
            l
        }
    }

    pub fn set_language(&self, lang: &str) {
        self.set_str("language", lang);
    }

    /// Whether the first-run setup (OOBE) has been completed.
    pub fn setup_done(&self) -> bool {
        self.load_map()
            .get("setup-done")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    pub fn set_setup_done(&self, done: bool) {
        self.mutate(|map| {
            map.insert("setup-done".to_owned(), json!(done));
        });
    }

    /// Whether this is a brand-new install (no config file yet). The OOBE
    /// shows only then — an existing config (e.g. an upgraded install) never
    /// triggers it, even if the `setup-done` marker is absent.
    pub fn is_first_run(&self) -> bool {
        !self.path.exists()
    }

    /// Wallpaper backend: "auto" (auto-detect, default), or a forced
    /// "gnome" / "kde" / "xfce". Unknown values are treated as "auto".
    pub fn backend(&self) -> String {
        let b = self.get_str("backend");
        if b.is_empty() {
            "auto".to_owned()
        } else {
            b
        }
    }

    pub fn set_backend(&self, backend: &str) {
        self.set_str("backend", backend);
    }

    /// Viewer remembered state (fullscreen window / fill mode), so the next
    /// fullscreen preview opens the way the user left it.
    fn viewer_bool(&self, key: &str) -> bool {
        self.load_map()
            .get(key)
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    fn set_viewer_bool(&self, key: &str, v: bool) {
        self.mutate(|map| {
            map.insert(key.to_owned(), json!(v));
        });
    }

    /// Whether the image viewer window was maximized last time.
    pub fn viewer_fullscreen(&self) -> bool {
        self.viewer_bool("viewer-fullscreen")
    }

    pub fn set_viewer_fullscreen(&self, v: bool) {
        self.set_viewer_bool("viewer-fullscreen", v);
    }

    /// Whether the image viewer was in "fill" (Cover crop) mode last time.
    pub fn viewer_fill(&self) -> bool {
        self.viewer_bool("viewer-fill")
    }

    pub fn set_viewer_fill(&self, v: bool) {
        self.set_viewer_bool("viewer-fill", v);
    }
}

/// Read the config; missing file returns an empty Map, parse failure returns Err.
fn load(path: &std::path::Path) -> Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let raw = std::fs::read_to_string(path).context("failed to read config file")?;
    let v: Value = serde_json::from_str(&raw).context("failed to parse config file")?;
    Ok(v.as_object().cloned().unwrap_or_default())
}

/// Atomic save: write a temp file then rename, so another process never reads
/// a half-written file.
fn save(path: &std::path::Path, map: &Map<String, Value>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(map)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, raw).context("failed to write config file")?;
    std::fs::rename(&tmp, path).context("failed to write config file")?;
    Ok(())
}

/// Keep the interval within sane bounds.
pub fn clamp_interval(secs: i64) -> i64 {
    secs.clamp(MIN_UPDATE_INTERVAL, MAX_UPDATE_INTERVAL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_interval() {
        assert_eq!(clamp_interval(3600), 3600);
        assert_eq!(clamp_interval(0), MIN_UPDATE_INTERVAL);
        assert_eq!(clamp_interval(-5), MIN_UPDATE_INTERVAL);
        assert_eq!(clamp_interval(i64::MAX), MAX_UPDATE_INTERVAL);
    }
}
