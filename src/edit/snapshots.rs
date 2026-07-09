//! Per-session snapshot store binding whole-file tags to the exact content that
//! minted them. Ported from oh-my-pi `packages/hashline/src/snapshots.ts` +
//! `packages/coding-agent/src/edit/file-snapshot-store.ts`.
//!
//! Backed by a byte-weighted `clru` LRU: a 64 MiB total ceiling across all
//! paths, at most 30 tracked paths (LRU eviction), a 4-version ring per path
//! (oldest dropped), and a 4 MiB per-file cap — a file over the cap mints no
//! tag (`record` returns `None`). Keys are canonical realpaths — `record`,
//! `invalidate`, and `relocate` normalize internally via
//! `crate::edit::normalize_path_key`.

#![allow(dead_code)]

use std::collections::hash_map::RandomState;
use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::path::Path;

use clru::{CLruCache, CLruCacheConfig, WeightScale};

use super::tag::compute_file_hash;

pub const DEFAULT_MAX_PATHS: usize = 30;
pub const DEFAULT_MAX_VERSIONS_PER_PATH: usize = 4;
pub const DEFAULT_MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_PER_FILE_CAP: usize = 4 * 1024 * 1024;

/// One full-file version observed at a point in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub path: String,
    pub text: String,
    pub tag: u16,
    /// Monotonic sequence number the version was recorded at (higher = newer).
    pub recorded_at: u64,
    /// 1-indexed file lines a producer actually displayed under this tag.
    pub seen_lines: HashSet<u32>,
}

/// Weigh a path's version history by the sum of its retained text bytes.
struct ByteScale;

impl WeightScale<String, Vec<Snapshot>> for ByteScale {
    #[allow(clippy::ptr_arg)]
    fn weight(&self, _key: &String, value: &Vec<Snapshot>) -> usize {
        1 + value
            .iter()
            .map(|s| s.text.len() + s.seen_lines.len() * std::mem::size_of::<u32>())
            .sum::<usize>()
    }
}

pub struct SnapshotStore {
    versions: CLruCache<String, Vec<Snapshot>, RandomState, ByteScale>,
    max_paths: usize,
    max_versions: usize,
    per_file_cap: usize,
    seq: u64,
}

impl Default for SnapshotStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotStore {
    pub fn new() -> Self {
        Self::with_limits(
            DEFAULT_MAX_TOTAL_BYTES,
            DEFAULT_MAX_PATHS,
            DEFAULT_MAX_VERSIONS_PER_PATH,
            DEFAULT_PER_FILE_CAP,
        )
    }

    pub fn with_limits(
        max_total_bytes: usize,
        max_paths: usize,
        max_versions: usize,
        per_file_cap: usize,
    ) -> Self {
        let cap = NonZeroUsize::new(max_total_bytes.max(1)).unwrap();
        let versions = CLruCache::with_config(CLruCacheConfig::new(cap).with_scale(ByteScale));
        Self {
            versions,
            max_paths,
            max_versions,
            per_file_cap,
            seq: 0,
        }
    }

    /// Record `full_text` for `path` and return its content tag. Returns `None`
    /// when the file exceeds the per-file cap (no tag is minted).
    pub fn record(
        &mut self,
        path: impl AsRef<Path>,
        full_text: &str,
        seen_lines: impl IntoIterator<Item = u32>,
    ) -> Option<u16> {
        if full_text.len() > self.per_file_cap {
            return None;
        }
        let key = crate::edit::normalize_path_key(path.as_ref());
        let tag = compute_file_hash(full_text);
        self.seq += 1;
        let seq = self.seq;
        let seen: HashSet<u32> = seen_lines.into_iter().collect();

        let mut history = self.versions.pop(&key).unwrap_or_default();
        if let Some(pos) = history
            .iter()
            .position(|s| s.tag == tag && s.text == full_text)
        {
            // Same content observed again: refresh recency, merge seen lines,
            // promote to head.
            let mut existing = history.remove(pos);
            existing.recorded_at = seq;
            existing.seen_lines.extend(seen);
            history.insert(0, existing);
        } else {
            history.insert(
                0,
                Snapshot {
                    path: key.clone(),
                    text: full_text.to_string(),
                    tag,
                    recorded_at: seq,
                    seen_lines: seen,
                },
            );
            history.truncate(self.max_versions);
        }
        // Best-effort store; if the single history exceeds the byte ceiling
        // (cannot happen under the per-file/version caps) the tag is still
        // returned but the snapshot is not retained.
        let _ = self.versions.put_with_weight(key, history);
        self.enforce_path_cap();
        Some(tag)
    }

    /// Merge `lines` into the seen-line set of the version tagged `tag`.
    pub fn record_seen_lines(
        &mut self,
        path: &str,
        tag: u16,
        lines: impl IntoIterator<Item = u32>,
    ) {
        let Some(mut history) = self.versions.pop(path) else {
            return;
        };
        if let Some(v) = history.iter_mut().find(|s| s.tag == tag) {
            v.seen_lines.extend(lines);
        }
        let _ = self.versions.put_with_weight(path.to_string(), history);
    }

    /// Most-recently recorded version for `path`.
    pub fn head(&self, path: &str) -> Option<Snapshot> {
        self.versions.peek(path).and_then(|h| h.first().cloned())
    }

    /// Tag of the most-recently recorded version for `path`, without cloning
    /// the (up to 4 MiB) snapshot text.
    pub fn head_tag(&self, path: &str) -> Option<u16> {
        self.versions
            .peek(path)
            .and_then(|h| h.first().map(|s| s.tag))
    }

    /// Retained version for `path` whose tag equals `tag`.
    pub fn by_tag(&self, path: &str, tag: u16) -> Option<Snapshot> {
        self.versions
            .peek(path)
            .and_then(|h| h.iter().find(|s| s.tag == tag).cloned())
    }

    /// Every retained version whose tag equals `tag`, across all tracked paths.
    pub fn find_by_tag(&self, tag: u16) -> Vec<Snapshot> {
        let mut out = Vec::new();
        for (_, history) in &self.versions {
            for s in history {
                if s.tag == tag {
                    out.push(s.clone());
                }
            }
        }
        out
    }

    /// Drop the version history for a single path.
    pub fn invalidate(&mut self, path: impl AsRef<Path>) {
        self.versions
            .pop(&crate::edit::normalize_path_key(path.as_ref()));
    }

    /// Move retained version history from `from` to `to`.
    pub fn relocate(&mut self, from: impl AsRef<Path>, to: impl AsRef<Path>) {
        let from_key = crate::edit::normalize_path_key(from.as_ref());
        let to_key = crate::edit::normalize_path_key(to.as_ref());
        let Some(source) = self.versions.pop(&from_key) else {
            return;
        };
        if source.is_empty() {
            return;
        }
        let relocated: Vec<Snapshot> = source
            .into_iter()
            .map(|s| Snapshot {
                path: to_key.clone(),
                ..s
            })
            .collect();
        let dest = self.versions.pop(&to_key);
        let merged = match dest {
            None => relocated,
            Some(dest) => {
                let mut seen = HashSet::new();
                let mut out = Vec::new();
                for s in relocated.into_iter().chain(dest) {
                    if seen.insert(s.tag) {
                        out.push(s);
                    }
                }
                out.truncate(self.max_versions);
                out
            }
        };
        let _ = self.versions.put_with_weight(to_key, merged);
        self.enforce_path_cap();
    }

    /// Number of tracked paths.
    pub fn len(&self) -> usize {
        self.versions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.versions.is_empty()
    }

    /// Total retained bytes (matches the weighted LRU's budget).
    pub fn total_weight(&self) -> usize {
        self.versions.weight()
    }

    pub fn clear(&mut self) {
        self.versions.clear();
    }

    fn enforce_path_cap(&mut self) {
        while self.versions.len() > self.max_paths {
            self.versions.pop_back();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_retrieve_by_tag() {
        let mut store = SnapshotStore::new();
        let tag = store
            .record(Path::new("a.rs"), "hello\nworld\n", [1, 2])
            .unwrap();
        let snap = store.by_tag("a.rs", tag).expect("retained");
        assert_eq!(snap.text, "hello\nworld\n");
        assert_eq!(snap.tag, tag);
        assert_eq!(snap.seen_lines, HashSet::from([1, 2]));
    }

    #[test]
    fn tag_collision_with_different_content_stores_both_versions() {
        let base = "line one\n";
        let base_tag = compute_file_hash(base);
        let mut colliding = None;
        for i in 0..200_000u32 {
            let cand = format!("candidate {i}\n");
            if compute_file_hash(&cand) == base_tag {
                colliding = Some(cand);
                break;
            }
        }
        let colliding = colliding.expect("16-bit collision found within search budget");

        let mut store = SnapshotStore::new();
        let t1 = store.record(Path::new("a.rs"), base, [1]).unwrap();
        let t2 = store.record(Path::new("a.rs"), &colliding, [1]).unwrap();
        assert_eq!(t1, t2, "both versions mint the same colliding tag");

        // The colliding-but-different version must be stored distinctly; by_tag
        // returns the latest content, not the stale first version.
        assert_eq!(store.by_tag("a.rs", t2).unwrap().text, colliding);
    }

    #[test]
    fn head_is_latest_version() {
        let mut store = SnapshotStore::new();
        store.record(Path::new("a.rs"), "v1\n", []).unwrap();
        let t2 = store.record(Path::new("a.rs"), "v2\n", []).unwrap();
        let head = store.head("a.rs").unwrap();
        assert_eq!(head.tag, t2);
        assert_eq!(head.text, "v2\n");
    }

    #[test]
    fn four_version_ring_drops_oldest() {
        let mut store = SnapshotStore::new();
        let t1 = store.record(Path::new("a.rs"), "1\n", []).unwrap();
        store.record(Path::new("a.rs"), "2\n", []).unwrap();
        store.record(Path::new("a.rs"), "3\n", []).unwrap();
        store.record(Path::new("a.rs"), "4\n", []).unwrap();
        // Fifth distinct version evicts the oldest (t1).
        let t5 = store.record(Path::new("a.rs"), "5\n", []).unwrap();
        assert!(store.by_tag("a.rs", t1).is_none(), "oldest version dropped");
        let head = store.head("a.rs").expect("head retained");
        assert_eq!(head.tag, t5, "head is the newest version");
        assert_eq!(head.text, "5\n");
    }

    #[test]
    fn ring_retains_exactly_the_newest_four() {
        let mut store = SnapshotStore::new();
        let t1 = store.record(Path::new("a.rs"), "1\n", []).unwrap();
        let t2 = store.record(Path::new("a.rs"), "2\n", []).unwrap();
        let t3 = store.record(Path::new("a.rs"), "3\n", []).unwrap();
        let t4 = store.record(Path::new("a.rs"), "4\n", []).unwrap();
        // At exactly 4 versions, all are retained.
        for t in [t1, t2, t3, t4] {
            assert!(
                store.by_tag("a.rs", t).is_some(),
                "4-version boundary: {t:04X}"
            );
        }
        // The 5th evicts only the oldest; the newest four remain.
        let t5 = store.record(Path::new("a.rs"), "5\n", []).unwrap();
        assert!(store.by_tag("a.rs", t1).is_none(), "oldest evicted");
        for t in [t2, t3, t4, t5] {
            assert!(
                store.by_tag("a.rs", t).is_some(),
                "newest four kept: {t:04X}"
            );
        }
    }

    #[test]
    fn per_file_cap_boundary_at_and_over() {
        // Cap of exactly 10 bytes: len == cap records, len == cap + 1 does not.
        let mut store = SnapshotStore::with_limits(64 * 1024 * 1024, 30, 4, 10);
        let at_cap = "x".repeat(10);
        let tag = store
            .record(Path::new("at.rs"), &at_cap, [])
            .expect("len == cap records");
        assert_eq!(store.by_tag("at.rs", tag).unwrap().text, at_cap);

        let over_cap = "x".repeat(11);
        assert_eq!(
            store.record(Path::new("over.rs"), &over_cap, []),
            None,
            "len == cap + 1 mints no tag"
        );
        assert!(
            store.head("over.rs").is_none(),
            "over-cap file not retained"
        );
    }

    #[test]
    fn per_file_cap_mints_no_tag() {
        let mut store = SnapshotStore::with_limits(64 * 1024 * 1024, 30, 4, 8);
        let big = "x".repeat(9); // > 8-byte cap
        assert_eq!(store.record(Path::new("big.rs"), &big, []), None);
        assert!(store.by_tag("big.rs", compute_file_hash(&big)).is_none());
        // A file under the cap still records.
        assert!(store.record(Path::new("small.rs"), "xy", []).is_some());
    }

    #[test]
    fn thirty_path_lru_evicts_coldest() {
        let mut store = SnapshotStore::new();
        for i in 0..30 {
            store
                .record(Path::new(&format!("f{i}.rs")), &format!("c{i}\n"), [])
                .unwrap();
        }
        assert_eq!(store.len(), 30);
        // Touch f0 so it is the most-recently used, then add one more path.
        let _ = store.head("f0.rs");
        // head() uses peek (non-mutating), so f0 is still coldest — but we can
        // bump recency with by_tag? by_tag also peeks. Record f0 again to bump.
        store.record(Path::new("f0.rs"), "c0\n", []).unwrap();
        store.record(Path::new("f30.rs"), "c30\n", []).unwrap();
        assert_eq!(store.len(), 30, "path count capped at 30");
        assert!(store.head("f0.rs").is_some(), "recently-used path retained");
        assert!(store.head("f1.rs").is_none(), "coldest path evicted");
    }

    #[test]
    fn byte_ceiling_enforced() {
        // Tiny ceiling: 100 bytes total. Each ~40-byte file; only a couple fit.
        let mut store = SnapshotStore::with_limits(100, 30, 4, 4 * 1024 * 1024);
        for i in 0..10 {
            store
                .record(Path::new(&format!("f{i}.rs")), &"z".repeat(40), [])
                .unwrap();
        }
        assert!(
            store.total_weight() <= 100,
            "total weight {} exceeds ceiling",
            store.total_weight()
        );
        // The ceiling forced eviction: not all 10 paths fit, and the most
        // recently recorded path is retained while an early one is gone.
        assert!(
            store.len() < 10,
            "ceiling evicted paths, len = {}",
            store.len()
        );
        assert!(store.head("f9.rs").is_some(), "newest path retained");
        assert!(store.head("f0.rs").is_none(), "coldest path evicted");
    }

    #[test]
    fn relocate_moves_history() {
        let mut store = SnapshotStore::new();
        let tag = store.record(Path::new("old.rs"), "content\n", [3]).unwrap();
        store.relocate(Path::new("old.rs"), Path::new("new.rs"));
        assert!(store.by_tag("old.rs", tag).is_none(), "source cleared");
        let moved = store.by_tag("new.rs", tag).expect("history at dest");
        assert_eq!(moved.path, "new.rs");
        assert_eq!(moved.text, "content\n");
    }

    #[test]
    fn record_seen_lines_merges() {
        let mut store = SnapshotStore::new();
        let tag = store
            .record(Path::new("a.rs"), "l1\nl2\nl3\n", [1])
            .unwrap();
        store.record_seen_lines("a.rs", tag, [2, 3]);
        let snap = store.by_tag("a.rs", tag).unwrap();
        assert_eq!(snap.seen_lines, HashSet::from([1, 2, 3]));
    }

    #[test]
    fn find_by_tag_across_paths() {
        let mut store = SnapshotStore::new();
        let tag = store.record(Path::new("a.rs"), "same\n", []).unwrap();
        // b.rs with identical content mints the same tag.
        assert_eq!(store.record(Path::new("b.rs"), "same\n", []).unwrap(), tag);
        let hits = store.find_by_tag(tag);
        assert_eq!(hits.len(), 2, "both paths carry the tag");
    }

    #[test]
    fn invalidate_drops_path() {
        let mut store = SnapshotStore::new();
        let tag = store.record(Path::new("a.rs"), "x\n", []).unwrap();
        store.invalidate(Path::new("a.rs"));
        assert!(store.by_tag("a.rs", tag).is_none());
    }

    #[test]
    fn same_content_rerecord_merges_seen_and_keeps_ring() {
        let mut store = SnapshotStore::new();
        let t1 = store.record(Path::new("a.rs"), "v1\n", []).unwrap();
        store.record(Path::new("a.rs"), "v2\n", []).unwrap();
        store.record(Path::new("a.rs"), "v3\n", []).unwrap();
        let t4 = store.record(Path::new("a.rs"), "v4\n", [1]).unwrap();

        // Re-recording identical content refreshes the SAME version in place
        // rather than inserting a duplicate.
        let again = store.record(Path::new("a.rs"), "v4\n", [2]).unwrap();
        assert_eq!(again, t4, "identical content mints the same tag");

        let snap = store.by_tag("a.rs", t4).unwrap();
        assert_eq!(
            snap.seen_lines,
            HashSet::from([1, 2]),
            "seen lines merged, not replaced"
        );
        assert_eq!(
            store.head("a.rs").unwrap().tag,
            t4,
            "re-record promotes to head"
        );
        // No ring slot consumed: the oldest distinct version is NOT evicted.
        assert!(
            store.by_tag("a.rs", t1).is_some(),
            "no duplicate inserted → v1 retained"
        );
    }
}
