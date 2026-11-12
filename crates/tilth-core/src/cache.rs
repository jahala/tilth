use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;

use crate::types::Lang;

/// The most memory the cached entries may hold, by [`Weigh::bytes`]. A
/// scripted MCP session of 105 reads, searches, callers, grok and deps calls
/// leaves 33 MiB of entries over the ripgrep fixture and 10 MiB over the
/// fastapi one (the server's RSS with the cache less its RSS without), so a
/// session over a tree that size keeps every entry. The same session over
/// the cargo registry source (1.8 GB) would leave 278 MiB. Within this
/// budget the cache starts again about twice there, and the session takes no
/// longer, because its time goes to the walks.
const CACHE_BUDGET_BYTES: usize = 128 * 1024 * 1024;

/// The heap a parsed tree takes per node `Node::descendant_count` counts.
/// That count leaves out hidden nodes, which take memory too, so the bytes
/// per counted node vary with the grammar. Over the 374 files of at least
/// 4 KB in the ripgrep, fastapi, gin and express fixtures, measured from the
/// allocator's bytes in use, a tree took 115 bytes per node for the median
/// file and at most 133 for nine files in ten. A tree costs about 30 times
/// its source, so a 235 KB Rust file holds 7 MB.
const TREE_BYTES_PER_NODE: usize = 128;

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

/// An entry's share of the cache's memory budget.
trait Weigh {
    /// The memory the entry for `path` holds: its slot in the table, its
    /// path, and what it owns. The table's spare slots are not counted.
    fn bytes(&self, path: &Path) -> usize;
}

impl Weigh for CacheEntry {
    fn bytes(&self, path: &Path) -> usize {
        std::mem::size_of::<(PathBuf, Self)>() + path.as_os_str().len() + self.outline.len()
    }
}

impl Weigh for ParsedEntry {
    fn bytes(&self, path: &Path) -> usize {
        std::mem::size_of::<(PathBuf, Self)>()
            + path.as_os_str().len()
            + self.file.content.len()
            + self.file.tree.root_node().descendant_count() * TREE_BYTES_PER_NODE
    }
}

/// Outline cache keyed by canonical path. A stale entry is replaced in O(1):
/// on every access the stored `mtime` is compared to the caller-supplied
/// value; a mismatch replaces the entry without scanning the whole map.
///
/// Stores two derived analyses: rendered outline strings (used by search
/// formatting) and parsed tree-sitter trees (used by AST scope queries).
/// Both share the same key + invalidation; nothing else is shared.
///
/// The MCP server keeps one cache for its whole life. Reads fill it with the
/// files they outline, and search with every file whose matches it shows, so
/// a long session over a large tree would otherwise hold a tree for every
/// file it ever showed. The entries stay within a memory budget instead.
/// When an entry takes them over it, the cache drops them all, that one
/// too, and starts again: a dropped file is outlined or parsed again on its
/// next use, as on its first.
pub struct OutlineCache {
    entries: DashMap<PathBuf, CacheEntry>,
    parsed: DashMap<PathBuf, ParsedEntry>,
    /// The bytes both tables hold, by [`Weigh::bytes`]. Each change to it is
    /// made under the lock of the shard that holds the entry it counts, so
    /// an entry's bytes come off only after they went on, and the count
    /// matches the tables whenever no call is part way through a change.
    bytes: AtomicUsize,
    budget: usize,
}

impl Default for OutlineCache {
    fn default() -> Self {
        Self::new()
    }
}

impl OutlineCache {
    #[must_use]
    pub fn new() -> Self {
        Self::with_budget(CACHE_BUDGET_BYTES)
    }

    fn with_budget(budget: usize) -> Self {
        Self {
            entries: DashMap::new(),
            parsed: DashMap::new(),
            bytes: AtomicUsize::new(0),
            budget,
        }
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
        let slot = self.entries.entry(path.to_path_buf());
        if let Entry::Occupied(ref occupied) = slot {
            if occupied.get().mtime == mtime {
                return Arc::clone(&occupied.get().outline);
            }
        }
        let outline: Arc<str> = compute().into();
        self.store(
            slot,
            CacheEntry {
                mtime,
                outline: Arc::clone(&outline),
            },
        );
        outline
    }

    /// Parse a code file with tree-sitter and cache the result. Returns
    /// `None` for non-code files, files larger than `source::MAX_FILE_SIZE`, or parse
    /// failures.
    ///
    /// Lookup and fill share one `entry()` for the same reason as
    /// [`Self::get_or_compute`]: concurrent callers would otherwise each parse
    /// the same cold file.
    #[must_use]
    pub fn get_or_parse(&self, path: &Path) -> Option<Arc<ParsedFile>> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta.modified().ok()?;
        if meta.len() > crate::source::MAX_FILE_SIZE {
            return None;
        }

        let slot = self.parsed.entry(path.to_path_buf());
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
        let content = crate::source::read_source(path).ok()?;
        let lang = crate::lang::refine_lang(lang, path, &content);
        let ts_lang = crate::lang::outline::outline_language(lang)?;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).ok()?;
        let tree = parser.parse(&content, None)?;
        let file = Arc::new(ParsedFile {
            content: Arc::new(content),
            tree,
            lang,
        });
        self.store(
            slot,
            ParsedEntry {
                mtime,
                file: Arc::clone(&file),
            },
        );
        Some(file)
    }

    /// Put `value` in `slot`, in place of any stale value there, then keep
    /// the cache within its budget.
    fn store<V: Weigh>(&self, slot: Entry<'_, PathBuf, V>, value: V) {
        let added = value.bytes(slot.key());
        // The count changes while `slot` still locks its shard, so
        // `drop_all`, which needs that lock to remove the entry, cannot take
        // its bytes off first.
        match slot {
            Entry::Occupied(mut stale) => {
                let freed = stale.get().bytes(stale.key());
                stale.insert(value);
                self.bytes.fetch_sub(freed, Ordering::Relaxed);
                self.bytes.fetch_add(added, Ordering::Relaxed);
            }
            Entry::Vacant(vacant) => {
                let filled = vacant.insert(value);
                self.bytes.fetch_add(added, Ordering::Relaxed);
                drop(filled);
            }
        }
        if self.bytes.load(Ordering::Relaxed) > self.budget {
            self.drop_all();
        }
    }

    /// Remove every entry of both tables, taking each one's bytes off the
    /// count.
    fn drop_all(&self) {
        drop_entries(&self.entries, &self.bytes);
        drop_entries(&self.parsed, &self.bytes);
    }
}

/// Remove every entry of `table`, taking each one's bytes off `bytes`. The
/// table locks one shard at a time, so a caller must hold none of them.
fn drop_entries<V: Weigh>(table: &DashMap<PathBuf, V>, bytes: &AtomicUsize) {
    table.retain(|path, entry| {
        bytes.fetch_sub(entry.bytes(path), Ordering::Relaxed);
        false
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    impl OutlineCache {
        /// How many entries the cache holds, and the bytes they take.
        ///
        /// The bytes are summed from the entries themselves, and must match
        /// the count the cache keeps.
        fn held(&self) -> (usize, usize) {
            let bytes = weigh(&self.entries) + weigh(&self.parsed);
            assert_eq!(
                self.bytes.load(Ordering::Relaxed),
                bytes,
                "the cache miscounts its bytes"
            );
            (self.entries.len() + self.parsed.len(), bytes)
        }
    }

    fn weigh<V: Weigh>(table: &DashMap<PathBuf, V>) -> usize {
        table
            .iter()
            .map(|entry| entry.value().bytes(entry.key()))
            .sum()
    }

    /// The path and outline of the `i`th file of a tree.
    fn nth_outline(i: usize) -> (PathBuf, String) {
        (
            PathBuf::from(format!("/tree/file_{i:03}.rs")),
            format!("fn f_{i:03}()"),
        )
    }

    /// A cache with room for about `files` outlines like those of [`nth_outline`].
    fn cache_with_room_for_outlines(files: usize) -> OutlineCache {
        let (path, outline) = nth_outline(0);
        let one = CacheEntry {
            mtime: SystemTime::UNIX_EPOCH,
            outline: outline.into(),
        }
        .bytes(&path);
        OutlineCache::with_budget(one * files)
    }

    #[test]
    fn outlines_stay_within_the_budget() {
        let cache = cache_with_room_for_outlines(10);
        for i in 0..100 {
            let (path, outline) = nth_outline(i);
            cache.get_or_compute(&path, SystemTime::UNIX_EPOCH, || outline);
            let (_, bytes) = cache.held();
            assert!(
                bytes <= cache.budget,
                "after {} outlines the cache holds {bytes} bytes, over its budget of {}",
                i + 1,
                cache.budget
            );
        }
        let (entries, _) = cache.held();
        assert!(entries <= 10, "the cache holds {entries} outlines");
    }

    #[test]
    fn dropped_outline_is_computed_again() {
        let cache = cache_with_room_for_outlines(10);
        let mtime = SystemTime::UNIX_EPOCH;
        for i in 0..100 {
            let (path, outline) = nth_outline(i);
            cache.get_or_compute(&path, mtime, || outline);
        }
        let (first, outline) = nth_outline(0);
        let mut computed = false;
        let again = cache.get_or_compute(&first, mtime, || {
            computed = true;
            outline.clone()
        });
        assert!(computed, "the first outline was not dropped");
        assert_eq!(&*again, outline);
    }

    /// Threads that fill and drop entries at once leave the count equal to
    /// the bytes the tables hold: each change to it is made under the lock of
    /// the shard it counts.
    #[test]
    fn count_stays_exact_under_concurrent_fills() {
        const THREADS: usize = 8;
        let cache = cache_with_room_for_outlines(10);
        std::thread::scope(|s| {
            for t in 0..THREADS {
                let cache = &cache;
                s.spawn(move || {
                    for i in 0..500 {
                        let (path, outline) = nth_outline((t * 500 + i) % 40);
                        let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs((i % 3) as u64);
                        cache.get_or_compute(&path, mtime, || outline);
                    }
                });
            }
        });
        let (_, bytes) = cache.held();
        assert!(bytes <= cache.budget, "the cache holds {bytes} bytes");
    }

    #[test]
    fn parsed_trees_stay_within_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<PathBuf> = (0..30)
            .map(|i| {
                let path = dir.path().join(format!("file_{i:02}.rs"));
                std::fs::write(
                    &path,
                    format!("fn f_{i:02}(x: u32) -> u32 {{ x + {i:02} }}\n"),
                )
                .unwrap();
                path
            })
            .collect();
        let one = {
            let measure = OutlineCache::new();
            measure.get_or_parse(&files[0]).unwrap();
            measure.held().1
        };
        let cache = OutlineCache::with_budget(one * 5);
        for (i, path) in files.iter().enumerate() {
            assert!(
                cache.get_or_parse(path).is_some(),
                "{} did not parse",
                path.display()
            );
            let (_, bytes) = cache.held();
            assert!(
                bytes <= cache.budget,
                "after {} trees the cache holds {bytes} bytes, over its budget of {}",
                i + 1,
                cache.budget
            );
        }
        let (trees, _) = cache.held();
        assert!(trees <= 5, "the cache holds {trees} trees");
    }

    #[test]
    fn evicts_stale_mtime_on_reinsert() {
        let cache = OutlineCache::new();
        let path = std::path::Path::new("fake/path.rs");
        let t0 = SystemTime::UNIX_EPOCH;
        let t1 = t0 + Duration::from_secs(1);

        // Insert with t0.
        cache.get_or_compute(path, t0, || "outline v0".to_string());
        assert_eq!(cache.held().0, 1);

        // Re-insert with t1 — stale t0 entry must be evicted, and its bytes
        // with it.
        cache.get_or_compute(path, t1, || "outline v1 is longer".to_string());
        assert_eq!(cache.held().0, 1, "stale entry was not evicted");

        // Confirm only the new entry survives.
        let hit = cache.get_or_compute(path, t1, || panic!("should hit cache"));
        assert_eq!(&*hit, "outline v1 is longer");
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
