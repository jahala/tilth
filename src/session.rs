use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::SystemTime;

use crate::edit::snapshots::SnapshotStore;

/// Tracks MCP activity across calls.
/// Stored alongside `OutlineCache` in server state.
pub struct Session {
    reads: AtomicUsize,
    searches: AtomicUsize,
    symbols: Mutex<HashMap<String, usize>>, // query → search count
    dir_hits: Mutex<HashMap<String, usize>>, // dir → count
    /// `path:line` → file mtime at expand-time. mtime versioning lets
    /// `is_expanded` detect stale records when the file has been edited
    /// since the expansion was first shown.
    expanded: Mutex<HashMap<String, SystemTime>>,
    /// Whole-file-tag snapshots bound to the content each edit-mode read
    /// displayed. Persists across `tilth_read`→`tilth_write` within a session
    /// so a follow-up edit can verify its tag and, on drift, 3-way-merge
    /// recover. Keyed by canonical realpath.
    snapshots: Mutex<SnapshotStore>,
}

impl Session {
    pub fn new() -> Self {
        Session {
            reads: AtomicUsize::new(0),
            searches: AtomicUsize::new(0),
            symbols: Mutex::new(HashMap::new()),
            dir_hits: Mutex::new(HashMap::new()),
            expanded: Mutex::new(HashMap::new()),
            snapshots: Mutex::new(SnapshotStore::new()),
        }
    }

    /// Lock the per-session snapshot store. Callers hold the guard for the
    /// duration of a record-or-recover operation. Prefer the narrow
    /// `record_snapshot` / `invalidate_snapshot` / `relocate_snapshot` methods
    /// for single operations; the raw guard is for the drift-recovery path,
    /// which needs `by_tag` and `try_recover` under one lock.
    pub fn snapshots(&self) -> MutexGuard<'_, SnapshotStore> {
        self.snapshots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Record a whole-file-tag snapshot, returning the minted tag (`None` when
    /// the file exceeds the per-file cap). Narrow read/emitter-side entrypoint —
    /// hands out no other store surface.
    pub fn record_snapshot(
        &self,
        key: &str,
        text: &str,
        seen_lines: impl IntoIterator<Item = u32>,
    ) -> Option<u16> {
        self.snapshots().record(key, text, seen_lines)
    }

    /// Drop the snapshot history for `key` (a removed file).
    pub fn invalidate_snapshot(&self, key: &str) {
        self.snapshots().invalidate(key);
    }

    /// Move the snapshot history from `from` to `to` (a renamed file).
    pub fn relocate_snapshot(&self, from: &str, to: &str) {
        self.snapshots().relocate(from, to);
    }

    pub fn record_read(&self, path: &Path) {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.record_dir(path);
    }

    #[cfg(test)]
    pub fn reads_count(&self) -> usize {
        self.reads.load(Ordering::Relaxed)
    }

    pub fn record_search(&self, query: &str) {
        self.searches.fetch_add(1, Ordering::Relaxed);
        let mut syms = self
            .symbols
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *syms.entry(query.to_string()).or_insert(0) += 1;
    }

    fn record_dir(&self, path: &Path) {
        if let Some(dir) = path.parent() {
            let key = dir.to_string_lossy().to_string();
            let mut dirs = self
                .dir_hits
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *dirs.entry(key).or_insert(0) += 1;
        }
    }

    /// Retained internal API for the recorded counters. The `tilth_session`
    /// MCP tool that called this was removed (undocumented drift); the read
    /// counters and `record_*` plumbing stay for reuse.
    #[allow(dead_code)]
    pub fn summary(&self) -> String {
        let reads = self.reads.load(Ordering::Relaxed);
        let searches = self.searches.load(Ordering::Relaxed);

        let mut out = format!("Files read: {reads} | Searches: {searches}");

        // Top symbols
        let syms = self
            .symbols
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !syms.is_empty() {
            let mut sorted: Vec<_> = syms.iter().collect();
            sorted.sort_by(|a, b| b.1.cmp(a.1));
            let top: Vec<String> = sorted
                .iter()
                .take(5)
                .map(|(name, count)| format!("{name} ({count})"))
                .collect();
            let _ = write!(out, "\nTop queries: {}", top.join(", "));
        }

        // Hot paths
        let dirs = self
            .dir_hits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !dirs.is_empty() {
            let mut sorted: Vec<_> = dirs.iter().collect();
            sorted.sort_by(|a, b| b.1.cmp(a.1));
            let top: Vec<String> = sorted
                .iter()
                .take(5)
                .map(|(dir, count)| format!("{dir} ({count})"))
                .collect();
            let _ = write!(out, "\nHot paths: {}", top.join(", "));
        }

        out
    }

    #[allow(dead_code)]
    pub fn reset(&self) {
        self.reads.store(0, Ordering::Relaxed);
        self.searches.store(0, Ordering::Relaxed);
        self.symbols
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.dir_hits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.expanded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.snapshots().clear();
    }

    /// Return true only when this `(path, line)` was previously expanded
    /// AND the recorded mtime matches `current_mtime`. After-edit re-grok
    /// falls back to a full re-inline.
    pub fn is_expanded(&self, path: &Path, line: u32, current_mtime: SystemTime) -> bool {
        let key = format!("{}:{}", path.display(), line);
        self.expanded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .is_some_and(|&recorded| recorded == current_mtime)
    }

    pub fn record_expand(&self, path: &Path, line: u32, mtime: SystemTime) {
        let key = format!("{}:{}", path.display(), line);
        self.expanded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, mtime);
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}
