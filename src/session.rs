use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

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
}

impl Session {
    pub fn new() -> Self {
        Self {
            reads: AtomicUsize::new(0),
            searches: AtomicUsize::new(0),
            symbols: Mutex::new(HashMap::new()),
            dir_hits: Mutex::new(HashMap::new()),
            expanded: Mutex::new(HashMap::new()),
        }
    }

    pub fn record_read(&self, path: &Path) {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.record_dir(path);
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

    pub fn summary(&self) -> String {
        let reads = self.reads.load(Ordering::Relaxed);
        let searches = self.searches.load(Ordering::Relaxed);

        let mut out = format!("Files read: {reads} | Searches: {searches}");

        let top = top_five(&self.symbols);
        if !top.is_empty() {
            let _ = write!(out, "\nTop queries: {}", top.join(", "));
        }
        let top = top_five(&self.dir_hits);
        if !top.is_empty() {
            let _ = write!(out, "\nHot paths: {}", top.join(", "));
        }

        out
    }

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

    /// Check-and-record in one lock acquisition: returns what
    /// [`Self::is_expanded`] would have returned, and records this expansion
    /// if it did not.
    ///
    /// Callers that check first and record after the body is assembled hold
    /// no lock in between, so two concurrent calls for the same target both
    /// see "not expanded" and both inline the full body — the dedup pays for
    /// itself only when exactly one caller can win the claim.
    pub fn claim_expand(&self, path: &Path, line: u32, mtime: SystemTime) -> bool {
        let key = format!("{}:{}", path.display(), line);
        let mut expanded = self
            .expanded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        expanded.insert(key, mtime) == Some(mtime)
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// The five highest counts, as `key (count)`, highest first.
fn top_five(counts: &Mutex<HashMap<String, usize>>) -> Vec<String> {
    let counts = counts
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut sorted: Vec<(&String, &usize)> = counts.iter().collect();
    sorted.sort_by_key(|&(_, &count)| std::cmp::Reverse(count));
    sorted
        .into_iter()
        .take(5)
        .map(|(key, count)| format!("{key} ({count})"))
        .collect()
}
