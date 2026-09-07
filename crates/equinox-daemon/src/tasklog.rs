//! Persistent task log: every queued task (scheduled/manual update, apply)
//! is recorded with start time, outcome, duration and error — the data behind
//! the GUI "update timeline" window. Stored as JSON at
//! `~/.local/share/equinox/tasks.json`, newest first, capped.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Log cap; the oldest entry is dropped beyond this.
pub const MAX_ENTRIES: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskLogEntry {
    pub id: u64,
    /// Unix timestamp when the task was enqueued.
    pub start_ts: i64,
    /// Unix timestamp when it finished; 0 while pending/running.
    pub end_ts: i64,
    /// User-visible (translated) label.
    pub label: String,
    /// "scheduled" | "manual" | "apply".
    pub kind: String,
    /// Source id (empty for non-source operations).
    pub source: String,
    /// True when the run caught up a window missed while off/asleep
    /// (elapsed ≥ 2× interval at fire time) or on first-ever run.
    pub catchup: bool,
    pub success: bool,
    /// Error message; empty on success.
    pub error: String,
    /// Progress of multi-step updates (backfill days): steps done / total.
    /// Zero total = not applicable.
    pub done: i32,
    pub total: i32,
    /// Number of automatic retries that happened before this entry finished
    /// (scheduled updates retry a failed fetch after 30 s, up to 3 times).
    /// Zero for entries that never retried. Old JSON without the field parses
    /// as zero via serde default.
    #[serde(default)]
    pub retries: i32,
}

pub struct TaskLog {
    path: PathBuf,
    entries: Vec<TaskLogEntry>,
}

impl TaskLog {
    pub fn load() -> Self {
        let path = glib::user_data_dir().join("equinox").join("tasks.json");
        let entries = if path.exists() {
            fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Self { path, entries }
    }

    pub fn entries(&self) -> &[TaskLogEntry] {
        &self.entries
    }

    /// Record a freshly enqueued task (newest first), persist.
    pub fn push(&mut self, entry: TaskLogEntry) {
        self.entries.insert(0, entry);
        self.entries.truncate(MAX_ENTRIES);
        let _ = self.save();
    }

    /// Update the progress counters of a running entry, persist.
    pub fn progress(&mut self, id: u64, done: i32, total: i32) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.id == id && e.end_ts == 0) {
            e.done = done;
            e.total = total;
            let _ = self.save();
        }
    }

    /// Record an automatic retry on a running entry, persist.
    pub fn retried(&mut self, id: u64) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.id == id && e.end_ts == 0) {
            e.retries += 1;
            let _ = self.save();
        }
    }

    /// Finalize a running/pending entry by id, persist.
    pub fn finish(&mut self, id: u64, success: bool, error: &str, end_ts: i64) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.id == id && e.end_ts == 0) {
            e.end_ts = end_ts;
            e.success = success;
            e.error = error.to_owned();
            let _ = self.save();
        }
    }

    fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, serde_json::to_string_pretty(&self.entries)?)?;
        Ok(())
    }
}
