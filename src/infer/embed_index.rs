//! Disk-cached per-repo embedding index for semantic file recall.
//!
//! Walks source files in a scope, builds a representative text per file
//! (relative path + outline symbol names + first ~1 KB of content), embeds
//! them with [`super::embed`], and persists the result under
//! `~/.cache/tilth/embed-index/<scope-hash>/` as a newline-delimited JSON
//! file.  Each line is `{"path":"…","mtime":<unix-secs>,"vec":[…]}`.
//!
//! On subsequent calls the cached file is read; entries whose on-disk mtime
//! differs from the stored mtime are re-embedded and the cache is updated.
//!
//! The index exposes [`CorpusEmbedIndex::recall`] which returns the top-k
//! files by cosine similarity to a prompt embedding.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::error::TilthError;
use crate::infer::{self, ModelConfig};
use crate::lang::detect_file_type;
use crate::lang::outline::get_outline_entries;
use crate::types::{FileType, OutlineKind};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Approximate byte cap for per-file content prefix fed to the embedder.
const CONTENT_PREFIX_BYTES: usize = 1024;

/// Maximum number of top-level symbol names included in the file text.
const MAX_SYMBOLS: usize = 48;

/// Cache file name. Version-bump when `file_text`'s format changes — vectors
/// embedded from an older text format must not be served as fresh (mtime alone
/// cannot detect a format change).
const INDEX_FILE: &str = "index-v2.ndjson";

/// Maximum file size we will read (mirrors locate's `MAX_FILE_BYTES`).
const MAX_FILE_BYTES: u64 = 500_000;

// ---------------------------------------------------------------------------
// Index entry (serialised)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct IndexEntry {
    path: PathBuf,
    /// Seconds since UNIX epoch of the file's mtime at index time.
    mtime: u64,
    vec: Vec<f32>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// A file-level embedding index for one repository scope.
///
/// Build with [`CorpusEmbedIndex::load_or_build`]; query with [`recall`].
pub struct CorpusEmbedIndex {
    entries: Vec<IndexEntry>,
}

impl CorpusEmbedIndex {
    /// Load the cached index for `scope`, rebuild stale entries, and return
    /// the index ready for [`recall`].
    ///
    /// `cfg` must point at the embedder model (use
    /// `ModelConfig::from_name("embedder")`).
    ///
    /// Returns `Err` only for non-recoverable failures (I/O errors writing the
    /// cache, or a hard model error).  A missing model returns
    /// `Err(InferError::ModelMissing)` wrapped in `TilthError`.
    pub fn load_or_build(cfg: &ModelConfig, scope: &Path) -> Result<Self, TilthError> {
        let cache_dir = index_cache_dir(scope)?;
        std::fs::create_dir_all(&cache_dir).map_err(|e| TilthError::InvalidQuery {
            query: cache_dir.to_string_lossy().into_owned(),
            reason: format!("cannot create embed-index cache dir: {e}"),
        })?;
        let cache_file = cache_dir.join(INDEX_FILE);

        // Load existing cached entries (if any).
        let mut cached: HashMap<PathBuf, IndexEntry> = load_cache(&cache_file);

        // Walk source files in the scope (mirrors locate's walker usage).
        let source_files = collect_source_files(scope)?;

        // Split into fresh (valid cache hit) and stale (need re-embedding).
        let mut fresh: Vec<IndexEntry> = Vec::new();
        let mut stale_paths: Vec<PathBuf> = Vec::new();
        let mut stale_texts: Vec<String> = Vec::new();

        for path in &source_files {
            let mtime = file_mtime(path).unwrap_or(0);
            if let Some(entry) = cached.remove(path) {
                if entry.mtime == mtime {
                    fresh.push(entry);
                    continue;
                }
            }
            // Stale or new — build representative text.
            if let Some(text) = file_text(path, scope) {
                stale_paths.push(path.clone());
                stale_texts.push(text);
            }
        }

        // Embed stale entries in one batch call (amortises model overhead).
        let mut rebuilt: Vec<IndexEntry> = Vec::new();
        if !stale_texts.is_empty() {
            let text_refs: Vec<&str> = stale_texts.iter().map(String::as_str).collect();
            let vecs = infer::embed(cfg, &text_refs).map_err(|e| TilthError::InvalidQuery {
                query: "embed".to_string(),
                reason: e.to_string(),
            })?;
            for (path, vec) in stale_paths.into_iter().zip(vecs) {
                let mtime = file_mtime(&path).unwrap_or(0);
                rebuilt.push(IndexEntry { path, mtime, vec });
            }
        }

        // Merge and persist.
        let mut all: Vec<IndexEntry> = fresh;
        all.extend(rebuilt);
        save_cache(&cache_file, &all);

        Ok(Self { entries: all })
    }

    /// Load the on-disk cache for `scope` WITHOUT walking or embedding. Returns
    /// `None` when no index has been built yet — callers degrade gracefully
    /// (locate-only pool) instead of triggering a slow full build in the query
    /// path. Build one explicitly via [`warm`].
    #[must_use]
    pub fn load_cached(scope: &Path) -> Option<Self> {
        let cache_file = index_cache_dir(scope).ok()?.join(INDEX_FILE);
        let cached = load_cache(&cache_file);
        if cached.is_empty() {
            return None;
        }
        Some(Self {
            entries: cached.into_values().collect(),
        })
    }

    /// Return the top-`k` files by cosine similarity to `prompt_vec`.
    ///
    /// `prompt_vec` must be a unit vector (L2-normalised) of the same
    /// dimension as the index vectors.  Returns `(relative-path, cosine)`
    /// pairs sorted descending by score.
    #[must_use]
    pub fn recall(&self, prompt_vec: &[f32], k: usize) -> Vec<(PathBuf, f32)> {
        if self.entries.is_empty() || k == 0 {
            return Vec::new();
        }
        let mut scored: Vec<(f32, &PathBuf)> = self
            .entries
            .iter()
            .map(|e| (cosine(prompt_vec, &e.vec), &e.path))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored
            .into_iter()
            .take(k)
            .map(|(score, path)| (path.clone(), score))
            .collect()
    }

    /// Score a specific set of paths by cosine similarity to `prompt_vec`.
    ///
    /// Unlike [`recall`], this does not sort or cap — it returns one score per
    /// path in the same order as `paths`.  Paths not present in the index get
    /// score `0.0`.  Used by the RRF ranker to score the pool without
    /// rebuilding the full recall list.
    #[must_use]
    pub fn score_paths(&self, prompt_vec: &[f32], scope: &Path, paths: &[&str]) -> Vec<f32> {
        // Index paths are absolute (walked from `scope`); callers pass
        // scope-relative paths (e.g. "src/foo.rs"). Key the lookup by the
        // relative path so the pool paths actually match.
        let lookup: std::collections::HashMap<String, f32> = self
            .entries
            .iter()
            .map(|e| {
                let rel = e
                    .path
                    .strip_prefix(scope)
                    .unwrap_or(&e.path)
                    .to_string_lossy()
                    .into_owned();
                (rel, cosine(prompt_vec, &e.vec))
            })
            .collect();
        paths
            .iter()
            .map(|p| lookup.get(*p).copied().unwrap_or(0.0))
            .collect()
    }

    /// Like [`score_paths`] but returns `None` for paths not in the index, so the
    /// caller can embed just those on-demand — keeping per-query cost bounded by
    /// pool size, not repo size.
    #[must_use]
    pub fn lookup_scores(
        &self,
        prompt_vec: &[f32],
        scope: &Path,
        paths: &[&str],
    ) -> Vec<Option<f32>> {
        let lookup: std::collections::HashMap<String, &Vec<f32>> = self
            .entries
            .iter()
            .map(|e| {
                let rel = e
                    .path
                    .strip_prefix(scope)
                    .unwrap_or(&e.path)
                    .to_string_lossy()
                    .into_owned();
                (rel, &e.vec)
            })
            .collect();
        paths
            .iter()
            .map(|p| lookup.get(*p).map(|v| cosine(prompt_vec, v)))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Cache helpers
// ---------------------------------------------------------------------------

/// Derive the cache directory for a scope:
/// `~/.cache/tilth/embed-index/<first-16-hex-chars-of-scope-hash>/`.
fn index_cache_dir(scope: &Path) -> Result<PathBuf, TilthError> {
    // Use a stable hash of the canonical scope path so renames bust the cache.
    let canonical = scope.canonicalize().unwrap_or_else(|_| scope.to_path_buf());
    let scope_str = canonical.to_string_lossy();
    let hash = fnv_hash(scope_str.as_bytes());
    let dir = home::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".cache")
        .join("tilth")
        .join("embed-index")
        .join(format!("{hash:016x}"));
    Ok(dir)
}

/// A simple 64-bit FNV-1a hash — no extra dep needed.
fn fnv_hash(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 14_695_981_039_346_656_037;
    const FNV_PRIME: u64 = 1_099_511_628_211;
    let mut h = FNV_OFFSET;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Load cached entries from the NDJSON file.  Silently ignores malformed lines.
fn load_cache(path: &Path) -> HashMap<PathBuf, IndexEntry> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<IndexEntry>(line).ok())
        .map(|e| (e.path.clone(), e))
        .collect()
}

/// Persist entries as NDJSON.  Silent on write failure (cache is best-effort).
fn save_cache(path: &Path, entries: &[IndexEntry]) {
    let mut lines = String::new();
    for e in entries {
        if let Ok(s) = serde_json::to_string(e) {
            lines.push_str(&s);
            lines.push('\n');
        }
    }
    let _ = std::fs::write(path, lines);
}

/// Return the file's mtime as seconds since UNIX epoch, or 0 on failure.
fn file_mtime(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

// ---------------------------------------------------------------------------
// File collection and text building
// ---------------------------------------------------------------------------

/// Cap on how many files the corpus index will embed. Bounds the warm-build
/// time on huge repos; the per-query gate does NOT depend on this (it scores its
/// pool on-demand), so a capped/partial corpus only bounds embed-recall.
const MAX_INDEX_FILES: usize = 3000;

/// Walk the source-code files to embed for the corpus index.
///
/// Unlike `search::walker` (which deliberately ignores `.gitignore` so grep can
/// reach generated files), the INDEX walker **respects `.gitignore`** — the
/// project's own declaration of "not source" is the most generic way to skip
/// deps / build output / generated trees across thousands of repos — plus the
/// fixed `SKIP_DIRS` junk list as a backstop for incomplete ignores. Capped at
/// `MAX_INDEX_FILES`.
fn collect_source_files(scope: &Path) -> Result<Vec<PathBuf>, TilthError> {
    use ignore::WalkBuilder;

    let collected = std::sync::Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let mut builder = WalkBuilder::new(scope);
    builder
        .follow_links(false) // don't chase symlinks out of the repo / into loops
        .same_file_system(true)
        .hidden(false)
        .git_ignore(true) // RESPECT .gitignore (the generic "not source" signal)
        .git_global(true)
        .git_exclude(true)
        .parents(true) // honour ignores from ancestor dirs too
        .filter_entry(|entry| {
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                if let Some(name) = entry.file_name().to_str() {
                    return !crate::search::SKIP_DIRS.contains(&name);
                }
            }
            true
        });
    builder.build_parallel().run(|| {
        let collected = std::sync::Arc::clone(&collected);
        Box::new(move |entry| {
            if let Ok(entry) = entry {
                if entry.file_type().is_some_and(|ft| ft.is_file()) {
                    let p = entry.path();
                    if is_embeddable(p) {
                        collected.lock().unwrap().push(p.to_path_buf());
                    }
                }
            }
            ignore::WalkState::Continue
        })
    });
    let mut paths = std::sync::Arc::try_unwrap(collected)
        .map(|m| m.into_inner().unwrap())
        .unwrap_or_default();
    paths.sort(); // deterministic order for stable cache
    paths.truncate(MAX_INDEX_FILES);
    Ok(paths)
}

/// True if the file is a source-code file we can embed.
fn is_embeddable(path: &Path) -> bool {
    if let Ok(m) = std::fs::metadata(path) {
        if m.len() > MAX_FILE_BYTES {
            return false;
        }
    }
    matches!(detect_file_type(path), FileType::Code(_))
}

/// Build a representative text for a file: `relative/path\n<symbols>\n<content-prefix>`.
///
/// Returns `None` if the file cannot be read.
fn file_text(path: &Path, scope: &Path) -> Option<String> {
    let FileType::Code(lang) = detect_file_type(path) else {
        return None;
    };

    let content = std::fs::read_to_string(path).ok()?;
    let rel = path
        .strip_prefix(scope)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();

    // Definition names from the outline, flattened so impl/class methods count
    // (top-level-only starves Rust/TS files of their real API surface).
    let entries = get_outline_entries(&content, lang);
    let mut flat: Vec<&crate::types::OutlineEntry> = Vec::new();
    crate::flatten_entries(&entries, &mut flat);
    let symbol_names: Vec<&str> = flat
        .iter()
        .filter(|e| !matches!(e.kind, OutlineKind::Import))
        .take(MAX_SYMBOLS)
        .map(|e| e.name.as_str())
        .collect();

    // Content prefix (first CONTENT_PREFIX_BYTES bytes of the file).
    let prefix = if content.len() > CONTENT_PREFIX_BYTES {
        // Truncate at a char boundary.
        let end = content
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i < CONTENT_PREFIX_BYTES)
            .last()
            .unwrap_or(CONTENT_PREFIX_BYTES.min(content.len()));
        &content[..end]
    } else {
        &content
    };

    let mut text = rel;
    if !symbol_names.is_empty() {
        text.push('\n');
        text.push_str(&symbol_names.join(" "));
    }
    text.push('\n');
    text.push_str(prefix);
    Some(text)
}

// ---------------------------------------------------------------------------
// Math
// ---------------------------------------------------------------------------

/// Cosine similarity of two vectors (both assumed unit-length).
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

// ---------------------------------------------------------------------------
// In-process cache: one CorpusEmbedIndex per (scope-path, model-path) pair
// stored in a static map so repeated calls within the same process skip disk.
// ---------------------------------------------------------------------------

static INDEX_CACHE: std::sync::OnceLock<Mutex<HashMap<(PathBuf, PathBuf), CorpusEmbedIndex>>> =
    std::sync::OnceLock::new();

fn index_cache() -> &'static Mutex<HashMap<(PathBuf, PathBuf), CorpusEmbedIndex>> {
    INDEX_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Recall top-k paths from the corpus index for `scope`, loading or building
/// the index as needed.
///
/// Returns an empty `Vec` (not an error) when:
/// - the `infer` feature is absent,
/// - the embedder model is missing, or
/// - the scope has no embeddable files.
///
/// Prints a one-line diagnostic to stderr for model-absent / model-error so
/// callers don't need to propagate the error.
#[must_use]
pub fn corpus_recall(
    cfg: &ModelConfig,
    scope: &Path,
    prompt_vec: &[f32],
    k: usize,
) -> Vec<(PathBuf, f32)> {
    let scope_key = scope.canonicalize().unwrap_or_else(|_| scope.to_path_buf());
    let cache_key = (scope_key.clone(), cfg.model_path.clone());

    // Check in-process cache first.
    {
        let guard = index_cache().lock().unwrap();
        if let Some(idx) = guard.get(&cache_key) {
            return idx.recall(prompt_vec, k);
        }
    }

    // Load the cached index only — NEVER build in the query path (a cold build
    // on a large repo is minutes). No warm index → no embed-recall; the gate
    // still works via on-demand pool scoring in `corpus_score_paths`.
    if let Some(idx) = CorpusEmbedIndex::load_cached(&scope_key) {
        let result = idx.recall(prompt_vec, k);
        index_cache().lock().unwrap().insert(cache_key, idx);
        result
    } else {
        Vec::new()
    }
}

/// Score specific `paths` (relative to `scope`) by cosine similarity to
/// `prompt_vec`, loading or building the index as needed.
///
/// Returns one `f32` per path in the same order.  Paths absent from the index
/// get `0.0`.  Returns all-zeros (not an error) when the index is unavailable.
#[must_use]
pub fn corpus_score_paths(
    cfg: &ModelConfig,
    scope: &Path,
    prompt_vec: &[f32],
    paths: &[&str],
) -> Vec<f32> {
    if paths.is_empty() {
        return Vec::new();
    }
    let scope_key = scope.canonicalize().unwrap_or_else(|_| scope.to_path_buf());
    let cache_key = (scope_key.clone(), cfg.model_path.clone());

    // Score against the cached index where present; embed any pool file the
    // index doesn't have ON-DEMAND. Per-query cost is bounded by pool size
    // (~a dozen files) regardless of repo size, and never triggers a full build.
    let mut opt: Vec<Option<f32>> = {
        let guard = index_cache().lock().unwrap();
        guard
            .get(&cache_key)
            .map(|idx| idx.lookup_scores(prompt_vec, &scope_key, paths))
    }
    .unwrap_or_else(|| {
        if let Some(idx) = CorpusEmbedIndex::load_cached(&scope_key) {
            let s = idx.lookup_scores(prompt_vec, &scope_key, paths);
            index_cache().lock().unwrap().insert(cache_key, idx);
            s
        } else {
            vec![None; paths.len()]
        }
    });

    let missing: Vec<usize> = opt
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.is_none().then_some(i))
        .collect();
    if !missing.is_empty() {
        let mut texts: Vec<String> = Vec::new();
        let mut idxs: Vec<usize> = Vec::new();
        for &i in &missing {
            if let Some(t) = file_text(&scope_key.join(paths[i]), &scope_key) {
                texts.push(t);
                idxs.push(i);
            }
        }
        if !texts.is_empty() {
            let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            if let Ok(vecs) = infer::embed(cfg, &refs) {
                for (i, v) in idxs.into_iter().zip(vecs) {
                    opt[i] = Some(cosine(prompt_vec, &v));
                }
            }
        }
    }

    opt.into_iter().map(|s| s.unwrap_or(0.0)).collect()
}

/// Explicitly build (or refresh) and cache the corpus index for `scope`. This is
/// the ONLY path that walks + embeds the whole corpus — the query path never
/// does. Safe to run ahead of time or in the background. Returns the file count.
pub fn warm(cfg: &ModelConfig, scope: &Path) -> Result<usize, TilthError> {
    let scope_key = scope.canonicalize().unwrap_or_else(|_| scope.to_path_buf());
    let idx = CorpusEmbedIndex::load_or_build(cfg, &scope_key)?;
    let n = idx.entries.len();
    let cache_key = (scope_key, cfg.model_path.clone());
    index_cache().lock().unwrap().insert(cache_key, idx);
    Ok(n)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("tilth_embed_index_{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn write_file(dir: &Path, name: &str, content: &str) {
        let mut f = std::fs::File::create(dir.join(name)).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn fnv_hash_is_stable() {
        let h1 = fnv_hash(b"hello");
        let h2 = fnv_hash(b"hello");
        assert_eq!(h1, h2);
        assert_ne!(fnv_hash(b"hello"), fnv_hash(b"world"));
    }

    #[test]
    fn cosine_unit_vectors() {
        let a = vec![1.0_f32, 0.0, 0.0];
        let b = vec![1.0_f32, 0.0, 0.0];
        let c = vec![0.0_f32, 1.0, 0.0];
        assert!((cosine(&a, &b) - 1.0).abs() < 1e-6, "self-cosine must be 1");
        assert!(cosine(&a, &c).abs() < 1e-6, "orthogonal cosine must be 0");
    }

    #[test]
    fn file_text_includes_path_and_content() {
        let dir = tmpdir("file_text");
        write_file(&dir, "foo.rs", "pub fn hello() {}\n");
        let text = file_text(&dir.join("foo.rs"), &dir).unwrap();
        assert!(text.contains("foo.rs"), "must include relative path");
        assert!(text.contains("hello"), "must include symbol name");
    }

    #[test]
    fn cache_roundtrip() {
        let dir = tmpdir("cache_rt");
        let cache_path = dir.join("index.ndjson");
        let entries = vec![IndexEntry {
            path: PathBuf::from("src/foo.rs"),
            mtime: 12345,
            vec: vec![0.1, 0.2, 0.3],
        }];
        save_cache(&cache_path, &entries);
        let loaded = load_cache(&cache_path);
        let e = loaded.get(&PathBuf::from("src/foo.rs")).unwrap();
        assert_eq!(e.mtime, 12345);
        assert!((e.vec[0] - 0.1).abs() < 1e-6);
    }

    #[test]
    fn recall_returns_top_k() {
        let idx = CorpusEmbedIndex {
            entries: vec![
                IndexEntry {
                    path: PathBuf::from("a.rs"),
                    mtime: 0,
                    vec: vec![1.0, 0.0],
                },
                IndexEntry {
                    path: PathBuf::from("b.rs"),
                    mtime: 0,
                    vec: vec![0.0, 1.0],
                },
                IndexEntry {
                    path: PathBuf::from("c.rs"),
                    mtime: 0,
                    vec: vec![0.707, 0.707],
                },
            ],
        };
        // Query aligned with a.rs.
        let q = vec![1.0_f32, 0.0];
        let top = idx.recall(&q, 2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].0, PathBuf::from("a.rs"));
        assert!((top[0].1 - 1.0).abs() < 1e-4);
    }
}
