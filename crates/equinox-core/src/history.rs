//! History of applied wallpapers, stored as JSON at `~/.local/share/equinox/history.json`.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// History cap; the oldest entry is dropped beyond this.
pub const MAX_ENTRIES: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub file: String,
    pub source: String,
    pub title: String,
    pub copyright: Option<String>,
    /// Unix timestamp.
    pub applied_at: i64,
}

/// History, newest first.
pub struct History {
    path: PathBuf,
    entries: Vec<HistoryEntry>,
}

impl History {
    pub fn load() -> Result<Self> {
        let path = glib::user_data_dir().join("equinox").join("history.json");
        let entries = if path.exists() {
            let raw = fs::read_to_string(&path).context("failed to read history file")?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok(Self { path, entries })
    }

    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    /// Current wallpaper (most recently applied).
    pub fn current(&self) -> Option<&HistoryEntry> {
        self.entries.first()
    }

    /// Append an entry (dedup by file path, existing moved to front), write to disk.
    pub fn push(&mut self, entry: HistoryEntry) -> Result<()> {
        if let Some(existing) = self.entries.iter().position(|e| e.file == entry.file) {
            self.entries.remove(existing);
        }
        self.entries.insert(0, entry);
        self.entries.truncate(MAX_ENTRIES);
        self.save()
    }

    /// Move an existing entry to the front (used by "previous"), write to disk.
    pub fn move_to_front(&mut self, idx: usize) -> Result<()> {
        if idx == 0 || idx >= self.entries.len() {
            return Ok(());
        }
        let entry = self.entries.remove(idx);
        self.entries.insert(0, entry);
        self.save()
    }

    /// Remove an entry, write to disk.
    pub fn remove(&mut self, idx: usize) -> Result<()> {
        if idx < self.entries.len() {
            self.entries.remove(idx);
            self.save()?;
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let raw = serde_json::to_string_pretty(&self.entries)?;
        fs::write(&self.path, raw).context("failed to write history file")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn temp_history() -> History {
        // Use a temp dir instead of the real user data dir, to avoid
        // polluting the environment.
        let dir = env::temp_dir().join(format!("equinox-test-{}", std::process::id()));
        let mut h = History {
            path: dir.join("history.json"),
            entries: Vec::new(),
        };
        h.entries = (0..3)
            .map(|i| HistoryEntry {
                file: format!("/tmp/img{i}.jpg"),
                source: "bing".into(),
                title: format!("Title {i}"),
                copyright: None,
                applied_at: 1_700_000_000 + i,
            })
            .collect();
        h
    }

    fn entry(i: i64) -> HistoryEntry {
        HistoryEntry {
            file: format!("/tmp/img{i}.jpg"),
            source: "bing".into(),
            title: format!("Title {i}"),
            copyright: None,
            applied_at: 1_700_000_000 + i,
        }
    }

    #[test]
    fn push_dedupes_by_file() {
        let mut h = temp_history();
        let old_len = h.entries().len();
        h.push(entry(1)).unwrap(); // already present → moved to front, length unchanged
        assert_eq!(h.entries().len(), old_len);
        assert_eq!(h.entries()[0].file, "/tmp/img1.jpg");
    }

    #[test]
    fn push_caps_at_max() {
        let mut h = History {
            path: env::temp_dir().join(format!("equinox-cap-{}", std::process::id())).join("h.json"),
            entries: Vec::new(),
        };
        for i in 0..(MAX_ENTRIES as i64 + 50) {
            h.push(entry(i)).unwrap();
        }
        assert_eq!(h.entries().len(), MAX_ENTRIES);
        // Oldest entry dropped
        assert!(!h.entries().iter().any(|e| e.file == "/tmp/img0.jpg"));
    }

    #[test]
    fn move_to_front_reorders() {
        let mut h = temp_history();
        h.move_to_front(2).unwrap();
        assert_eq!(h.entries()[0].file, "/tmp/img2.jpg");
        assert_eq!(h.entries().len(), 3);
    }

    #[test]
    fn remove_drops_entry() {
        let mut h = temp_history();
        h.remove(1).unwrap();
        assert_eq!(h.entries().len(), 2);
        assert!(!h.entries().iter().any(|e| e.file == "/tmp/img1.jpg"));
    }

    #[test]
    fn persistence_roundtrip() {
        let dir = env::temp_dir().join(format!("equinox-persist-{}", std::process::id()));
        let path = dir.join("history.json");
        let mut h = History { path: path.clone(), entries: Vec::new() };
        h.push(entry(7)).unwrap();
        drop(h);
        let h2 = History::load_from(path);
        assert_eq!(h2.entries().len(), 1);
        assert_eq!(h2.entries()[0].file, "/tmp/img7.jpg");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
impl History {
    fn load_from(path: PathBuf) -> Self {
        let entries = if path.exists() {
            let raw = fs::read_to_string(&path).unwrap_or_default();
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            Vec::new()
        };
        Self { path, entries }
    }
}
