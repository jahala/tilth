use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use dashmap::DashMap;

use crate::types::Lang;

/// Cached outline entry keyed by path. `mtime` is stored in the value so a
/// stale entry is detected and evicted in O(1) (one map lookup, no `retain`).
struct CacheEntry {
    mtime: SystemTime,
    outline: Arc<str>,
}

/// File contents and its tree-sitter parse, cached together so AST consumers
/// don't re-parse on every call. `content` is `Arc<String>` so callers can
/// hold the bytes for `Node::utf8_text` without copying.
pub struct ParsedFile {
    pub content: Arc<String>,
    pub tree: tree_sitter::Tree,
    pub lang: Lang,
}

/// Cached parsed entry keyed by path.
struct ParsedEntry {
    mtime: SystemTime,
    file: Arc<ParsedFile>,
}

/// Outline cache keyed by canonical path. Eviction is O(1): on every access
/// the stored `mtime` is compared to the caller-supplied value; a mismatch
/// replaces the entry without scanning the whole map.
///
/// Stores two derived analyses: rendered outline strings (used by search
/// formatting) and parsed tree-sitter trees (used by AST scope queries).
/// Both share the same key + invalidation; nothing else is shared.
pub struct OutlineCache {
    entries: DashMap<PathBuf, CacheEntry>,
    parsed: DashMap<PathBuf, ParsedEntry>,
}

impl Default for OutlineCache {
    fn default() -> Self {
        Self {
            entries: DashMap::new(),
            parsed: DashMap::new(),
        }
    }
}

impl OutlineCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Get cached outline or compute and cache it. Accepts `&Path` (not `&PathBuf`).
    ///
    /// The lookup and the fill are one `entry()` operation: a get-then-insert
    /// leaves a window in which every concurrent caller also misses, so N
    /// threads asking for the same cold file all pay for the outline. `compute`
    /// runs while the entry is held, so it MUST NOT call back into this cache
    /// — the shard lock is not reentrant.
    pub fn get_or_compute(
        &self,
        path: &Path,
        mtime: SystemTime,
        compute: impl FnOnce() -> String,
    ) -> Arc<str> {
        use dashmap::mapref::entry::Entry;

        match self.entries.entry(path.to_path_buf()) {
            // Fresh entry — hand back the cached outline.
            Entry::Occupied(slot) if slot.get().mtime == mtime => Arc::clone(&slot.get().outline),
            // Stale entry — recompute in place, evicting the old outline.
            Entry::Occupied(mut slot) => {
                let outline: Arc<str> = compute().into();
                slot.insert(CacheEntry {
                    mtime,
                    outline: Arc::clone(&outline),
                });
                outline
            }
            Entry::Vacant(slot) => {
                let outline: Arc<str> = compute().into();
                slot.insert(CacheEntry {
                    mtime,
                    outline: Arc::clone(&outline),
                });
                outline
            }
        }
    }

    /// Parse a code file with tree-sitter and cache the result. Returns
    /// `None` for non-code files, files larger than the 500 KB cap, or parse
    /// failures.
    ///
    /// Lookup and fill share one `entry()` for the same reason as
    /// [`Self::get_or_compute`]: concurrent callers would otherwise each parse
    /// the same cold file.
    #[must_use]
    pub fn get_or_parse(&self, path: &Path) -> Option<Arc<ParsedFile>> {
        use dashmap::mapref::entry::Entry;

        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta.modified().ok()?;
        if meta.len() > 500_000 {
            return None;
        }

        let mut slot = self.parsed.entry(path.to_path_buf());
        if let Entry::Occupied(ref occupied) = slot {
            if occupied.get().mtime == mtime {
                return Some(Arc::clone(&occupied.get().file));
            }
        }

        // Stale or absent — parse and fill the held entry. Every `?` below
        // leaves a stale entry in place rather than evicting it; the next
        // caller retries the parse against the same mtime check.
        let crate::types::FileType::Code(lang) = crate::lang::detect_file_type(path) else {
            return None;
        };
        let ts_lang = crate::lang::outline::outline_language(lang)?;
        let content = std::fs::read_to_string(path).ok()?;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).ok()?;
        let tree = parser.parse(&content, None)?;
        let file = Arc::new(ParsedFile {
            content: Arc::new(content),
            tree,
            lang,
        });
        let entry = ParsedEntry {
            mtime,
            file: Arc::clone(&file),
        };
        match slot {
            Entry::Occupied(ref mut occupied) => {
                occupied.insert(entry);
            }
            Entry::Vacant(vacant) => {
                vacant.insert(entry);
            }
        }
        Some(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    #[test]
    fn evicts_stale_mtime_on_reinsert() {
        let cache = OutlineCache::new();
        let path = std::path::Path::new("fake/path.rs");
        let t0 = SystemTime::UNIX_EPOCH;
        let t1 = t0 + Duration::from_secs(1);

        // Insert with t0.
        cache.get_or_compute(path, t0, || "outline v0".to_string());
        assert_eq!(cache.entries.len(), 1);

        // Re-insert with t1 — stale t0 entry must be evicted.
        cache.get_or_compute(path, t1, || "outline v1".to_string());
        assert_eq!(cache.entries.len(), 1, "stale entry was not evicted");

        // Confirm only the new entry survives.
        let hit = cache.get_or_compute(path, t1, || panic!("should hit cache"));
        assert_eq!(&*hit, "outline v1");
    }

    /// A get-then-insert cache has a window between the miss and the insert in
    /// which every other caller also misses, so N threads asking for the same
    /// cold key all pay for the outline — the exact work the cache exists to
    /// avoid. The lookup and the fill must be one atomic operation.
    #[test]
    fn get_or_compute_computes_once_under_concurrent_callers() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        const THREADS: usize = 8;
        for i in 0..40 {
            let cache = OutlineCache::new();
            let path = std::path::Path::new("fake/concurrent.rs");
            let mtime = SystemTime::UNIX_EPOCH;
            let computes = AtomicUsize::new(0);
            let barrier = std::sync::Barrier::new(THREADS);

            std::thread::scope(|s| {
                for _ in 0..THREADS {
                    s.spawn(|| {
                        barrier.wait();
                        cache.get_or_compute(path, mtime, || {
                            computes.fetch_add(1, Ordering::SeqCst);
                            "outline".to_string()
                        });
                    });
                }
            });

            assert_eq!(
                computes.load(Ordering::SeqCst),
                1,
                "iteration {i}: {THREADS} concurrent callers must compute the outline once"
            );
        }
    }
}
