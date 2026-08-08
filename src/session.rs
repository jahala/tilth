use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::SystemTime;

use serde_json::Value;

use crate::edit::snapshots::SnapshotStore;

const BATCH_NUDGE_LIMIT: u8 = 2;
const BATCH_NUDGE_MAX_CHARS: usize = 120;

#[derive(Clone, Copy, PartialEq, Eq)]
enum BatchTool {
    Read,
    Search,
    List,
}

impl BatchTool {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "tilth_read" => Some(Self::Read),
            "tilth_search" => Some(Self::Search),
            "tilth_list" => Some(Self::List),
            _ => None,
        }
    }

    fn parameter(self) -> &'static str {
        match self {
            Self::Read => "paths",
            Self::Search => "queries",
            Self::List => "patterns",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Read => 0,
            Self::Search => 1,
            Self::List => 2,
        }
    }

    fn generic_tip(self) -> &'static str {
        match self {
            Self::Read => "TIP: batch into one call — paths: [\"a.rs\", \"b.rs\"].",
            Self::Search => {
                "TIP: batch into one call — queries: [{\"query\":\"foo\"}, {\"query\":\"bar\"}]."
            }
            Self::List => "TIP: batch into one call — patterns: [\"*.rs\", \"*.toml\"].",
        }
    }
}

#[derive(Default)]
struct BatchNudgeState {
    previous: Option<(BatchTool, Value)>,
    emissions: [u8; 3],
}

impl BatchNudgeState {
    /// Record one dispatched call in the streak. `emit` is false for calls
    /// whose response cannot carry a tip (errored dispatches): the streak
    /// still advances, but no tip is produced and no emission is consumed.
    fn record(&mut self, tool_name: &str, args: &Value, emit: bool) -> Option<String> {
        let Some(tool) = BatchTool::from_name(tool_name) else {
            self.previous = None;
            return None;
        };
        let Some(items) = args.get(tool.parameter()).and_then(Value::as_array) else {
            self.previous = None;
            return None;
        };
        if items.len() != 1 {
            self.previous = None;
            return None;
        }

        let current = items[0].clone();
        let previous = self.previous.replace((tool, current.clone()));
        let (_, previous_item) = previous.filter(|(last, _)| *last == tool)?;
        if !emit {
            return None;
        }
        // A repeat of the same item would render a nonsense self-batch tip
        // ("[a.rs, a.rs]"); re-reads with changed options are ordinary agent
        // behavior, so skip without spending an emission.
        if previous_item == current {
            return None;
        }

        let emissions = &mut self.emissions[tool.index()];
        if *emissions >= BATCH_NUDGE_LIMIT {
            return None;
        }
        *emissions += 1;

        let tip = format!(
            "TIP: batch into one call — {}: [{previous_item}, {current}].",
            tool.parameter()
        );
        if tip.chars().count() <= BATCH_NUDGE_MAX_CHARS {
            Some(tip)
        } else {
            Some(tool.generic_tip().to_string())
        }
    }
}

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
    /// Cumulative token estimates: sum of full-file baseline tokens and
    /// tokens actually returned across all reads in this session.
    baseline_tokens: AtomicU64,
    saved_tokens: AtomicU64,
    batch_nudges: Mutex<BatchNudgeState>,
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
            baseline_tokens: AtomicU64::new(0),
            saved_tokens: AtomicU64::new(0),
            batch_nudges: Mutex::new(BatchNudgeState::default()),
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
        path: &Path,
        text: &str,
        seen_lines: impl IntoIterator<Item = u32>,
    ) -> Option<u16> {
        self.snapshots().record(path, text, seen_lines)
    }

    /// Drop the snapshot history for `path` (a removed file).
    pub fn invalidate_snapshot(&self, path: &Path) {
        self.snapshots().invalidate(path);
    }

    /// Move the snapshot history from `from` to `to` (a renamed file).
    pub fn relocate_snapshot(&self, from: &Path, to: &Path) {
        self.snapshots().relocate(from, to);
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

    /// Observe one dispatched MCP tool call (every dispatch, errored or not —
    /// skipping errored calls would let a failed intervening tool preserve a
    /// streak it actually broke) and return a just-in-time batching tip when
    /// the same batchable tool receives consecutive single-item arrays.
    /// `emit` is false when the response cannot carry a tip.
    pub fn batch_nudge(&self, tool: &str, args: &Value, emit: bool) -> Option<String> {
        self.batch_nudges
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record(tool, args, emit)
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
    /// The `tilth_savings` MCP tool that surfaced this was cut (see
    /// paulnsorensen/tilth ticket); the counters and `record_savings` plumbing
    /// stay for a future CLI/reporting decision.
    #[allow(dead_code)]
    pub fn savings(&self) -> (u64, u64) {
        (
            self.baseline_tokens.load(Ordering::Relaxed),
            self.saved_tokens.load(Ordering::Relaxed),
        )
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

        *self
            .batch_nudges
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = BatchNudgeState::default();
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
    fn second_consecutive_single_item_call_returns_batch_nudge() {
        let session = Session::new();

        assert_eq!(
            session.batch_nudge(
                "tilth_read",
                &serde_json::json!({ "paths": ["a.go"] }),
                true
            ),
            None
        );
        assert_eq!(
            session.batch_nudge(
                "tilth_read",
                &serde_json::json!({ "paths": ["b.go"] }),
                true
            ),
            Some("TIP: batch into one call — paths: [\"a.go\", \"b.go\"].".to_string())
        );
    }

    #[test]
    fn multi_item_call_resets_batch_nudge_streak() {
        let session = Session::new();

        assert_eq!(
            session.batch_nudge(
                "tilth_list",
                &serde_json::json!({ "patterns": ["*.rs"] }),
                true
            ),
            None
        );
        assert_eq!(
            session.batch_nudge(
                "tilth_list",
                &serde_json::json!({ "patterns": ["*.rs", "*.toml"] }),
                true,
            ),
            None
        );
        assert_eq!(
            session.batch_nudge(
                "tilth_list",
                &serde_json::json!({ "patterns": ["*.md"] }),
                true
            ),
            None
        );
        assert_eq!(
            session.batch_nudge(
                "tilth_list",
                &serde_json::json!({ "patterns": ["*.txt"] }),
                true
            ),
            Some("TIP: batch into one call — patterns: [\"*.md\", \"*.txt\"].".to_string())
        );
    }

    #[test]
    fn different_tool_resets_batch_nudge_streak() {
        let session = Session::new();

        assert_eq!(
            session.batch_nudge(
                "tilth_read",
                &serde_json::json!({ "paths": ["a.rs"] }),
                true
            ),
            None
        );
        assert_eq!(
            session.batch_nudge(
                "tilth_search",
                &serde_json::json!({ "queries": [{ "query": "needle" }] }),
                true,
            ),
            None
        );
        assert_eq!(
            session.batch_nudge(
                "tilth_read",
                &serde_json::json!({ "paths": ["b.rs"] }),
                true
            ),
            None
        );
        assert_eq!(
            session.batch_nudge(
                "tilth_read",
                &serde_json::json!({ "paths": ["c.rs"] }),
                true
            ),
            Some("TIP: batch into one call — paths: [\"b.rs\", \"c.rs\"].".to_string())
        );
    }

    #[test]
    fn batch_nudge_emits_at_most_twice_per_tool() {
        let session = Session::new();
        let calls = ["one", "two", "three", "four"];

        let nudges: Vec<_> = calls
            .into_iter()
            .map(|query| {
                session.batch_nudge(
                    "tilth_search",
                    &serde_json::json!({ "queries": [{ "query": query }] }),
                    true,
                )
            })
            .collect();

        assert_eq!(
            nudges,
            vec![
                None,
                Some(
                    "TIP: batch into one call — queries: [{\"query\":\"one\"}, {\"query\":\"two\"}]."
                        .to_string()
                ),
                Some(
                    "TIP: batch into one call — queries: [{\"query\":\"two\"}, {\"query\":\"three\"}]."
                        .to_string()
                ),
                None,
            ]
        );
    }

    #[test]
    fn multi_item_calls_never_emit_batch_nudge() {
        let session = Session::new();
        let args = serde_json::json!({ "paths": ["a.rs", "b.rs"] });

        assert_eq!(session.batch_nudge("tilth_read", &args, true), None);
        assert_eq!(session.batch_nudge("tilth_read", &args, true), None);
        assert_eq!(session.batch_nudge("tilth_read", &args, true), None);
    }

    #[test]
    fn repeating_the_same_item_never_tips_and_preserves_the_budget() {
        // A re-read of the same file (changed budget/mode) must not render a
        // nonsense self-batch tip, and must not spend an emission: the full
        // 2-emission budget stays available for real advice afterwards.
        let session = Session::new();
        let same = serde_json::json!({ "paths": ["a.rs"] });

        assert_eq!(session.batch_nudge("tilth_read", &same, true), None);
        assert_eq!(session.batch_nudge("tilth_read", &same, true), None);
        assert_eq!(session.batch_nudge("tilth_read", &same, true), None);
        assert_eq!(
            session.batch_nudge(
                "tilth_read",
                &serde_json::json!({ "paths": ["b.rs"] }),
                true
            ),
            Some("TIP: batch into one call — paths: [\"a.rs\", \"b.rs\"].".to_string())
        );
        assert_eq!(
            session.batch_nudge(
                "tilth_read",
                &serde_json::json!({ "paths": ["c.rs"] }),
                true
            ),
            Some("TIP: batch into one call — paths: [\"b.rs\", \"c.rs\"].".to_string())
        );
    }

    #[test]
    fn non_batchable_tool_resets_batch_nudge_streak() {
        // The from_name → None arm must reset `previous`, not merely skip.
        let session = Session::new();

        assert_eq!(
            session.batch_nudge(
                "tilth_read",
                &serde_json::json!({ "paths": ["a.rs"] }),
                true
            ),
            None
        );
        assert_eq!(
            session.batch_nudge("tilth_grok", &serde_json::json!({ "target": "X" }), true),
            None
        );
        assert_eq!(
            session.batch_nudge(
                "tilth_read",
                &serde_json::json!({ "paths": ["b.rs"] }),
                true
            ),
            None
        );
    }

    #[test]
    fn errored_call_advances_streak_without_tipping_or_spending_budget() {
        // emit=false (errored dispatch): the call still joins the streak, so
        // the next successful call tips against it, and no emission is spent
        // on the suppressed tip.
        let session = Session::new();

        assert_eq!(
            session.batch_nudge(
                "tilth_read",
                &serde_json::json!({ "paths": ["a.rs"] }),
                true
            ),
            None
        );
        assert_eq!(
            session.batch_nudge(
                "tilth_read",
                &serde_json::json!({ "paths": ["b.rs"] }),
                false
            ),
            None
        );
        assert_eq!(
            session.batch_nudge(
                "tilth_read",
                &serde_json::json!({ "paths": ["c.rs"] }),
                true
            ),
            Some("TIP: batch into one call — paths: [\"b.rs\", \"c.rs\"].".to_string())
        );
        assert_eq!(
            session.batch_nudge(
                "tilth_read",
                &serde_json::json!({ "paths": ["d.rs"] }),
                true
            ),
            Some("TIP: batch into one call — paths: [\"c.rs\", \"d.rs\"].".to_string())
        );
    }

    #[test]
    fn oversized_tip_falls_back_to_the_generic_form() {
        let session = Session::new();
        let long_a = format!("src/{}/mod.rs", "a".repeat(60));
        let long_b = format!("src/{}/mod.rs", "b".repeat(60));

        assert_eq!(
            session.batch_nudge(
                "tilth_read",
                &serde_json::json!({ "paths": [long_a] }),
                true
            ),
            None
        );
        assert_eq!(
            session.batch_nudge(
                "tilth_read",
                &serde_json::json!({ "paths": [long_b] }),
                true
            ),
            Some("TIP: batch into one call — paths: [\"a.rs\", \"b.rs\"].".to_string())
        );
    }

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
