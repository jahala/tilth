use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
    /// Cumulative token estimates: sum of full-file baseline tokens and
    /// tokens actually returned across all reads in this session.
    baseline_tokens: AtomicU64,
    saved_tokens: AtomicU64,
}

impl Session {
    pub fn new() -> Self {
        Session {
            reads: AtomicUsize::new(0),
            searches: AtomicUsize::new(0),
            symbols: Mutex::new(HashMap::new()),
            dir_hits: Mutex::new(HashMap::new()),
            expanded: Mutex::new(HashMap::new()),
            baseline_tokens: AtomicU64::new(0),
            saved_tokens: AtomicU64::new(0),
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

    /// Record a read event for savings accounting.
    /// `baseline_tokens`: estimated tokens for the full file (naive read).
    /// `returned_tokens`: estimated tokens for what tilth actually returned.
    /// Per-event clamp via `saturating_sub` ensures saved is never negative.
    pub fn record_savings(&self, baseline_tokens: u64, returned_tokens: u64) {
        self.baseline_tokens
            .fetch_add(baseline_tokens, Ordering::Relaxed);
        self.saved_tokens.fetch_add(
            baseline_tokens.saturating_sub(returned_tokens),
            Ordering::Relaxed,
        );
    }

    /// Returns `(baseline_tokens, saved_tokens)` accumulated this session.
    pub fn savings(&self) -> (u64, u64) {
        (
            self.baseline_tokens.load(Ordering::Relaxed),
            self.saved_tokens.load(Ordering::Relaxed),
        )
    }

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
        self.baseline_tokens.store(0, Ordering::Relaxed);
        self.saved_tokens.store(0, Ordering::Relaxed);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_savings_accumulates_across_calls() {
        let session = Session::new();
        session.record_savings(1000, 200);
        session.record_savings(500, 100);
        let (baseline, saved) = session.savings();
        assert_eq!(baseline, 1500);
        assert_eq!(saved, 1200); // (1000-200) + (500-100)
    }

    #[test]
    fn record_savings_clamps_when_returned_exceeds_baseline() {
        let session = Session::new();
        // returned > baseline: saved contribution is 0, baseline still accumulates
        session.record_savings(100, 500);
        let (baseline, saved) = session.savings();
        assert_eq!(baseline, 100);
        assert_eq!(saved, 0);
    }

    #[test]
    fn record_savings_exact_match_adds_zero_saved() {
        let session = Session::new();
        session.record_savings(400, 400);
        let (baseline, saved) = session.savings();
        assert_eq!(baseline, 400);
        assert_eq!(saved, 0);
    }

    #[test]
    fn savings_getter_returns_both_counters() {
        let session = Session::new();
        let (b, s) = session.savings();
        assert_eq!(b, 0);
        assert_eq!(s, 0);
        session.record_savings(300, 50);
        let (b2, s2) = session.savings();
        assert_eq!(b2, 300);
        assert_eq!(s2, 250);
    }

    #[test]
    fn reset_zeroes_savings_counters() {
        let session = Session::new();
        session.record_savings(1000, 100);
        let (b, s) = session.savings();
        assert!(
            b > 0 && s > 0,
            "precondition: counters non-zero before reset"
        );
        session.reset();
        let (b2, s2) = session.savings();
        assert_eq!(b2, 0, "baseline_tokens must be zero after reset");
        assert_eq!(s2, 0, "saved_tokens must be zero after reset");
    }
}
