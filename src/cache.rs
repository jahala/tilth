use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;

use crate::types::Lang;

/// Cached outline entry.
struct CacheEntry {
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

/// Outline cache keyed by (canonical path, mtime). If the file changes,
/// mtime changes and the old entry is never hit again.
///
/// Stores two derived analyses: rendered outline strings (used by search
/// formatting) and parsed tree-sitter trees (used by AST scope queries).
/// Both share the same key + invalidation; nothing else is shared.
pub struct OutlineCache {
    entries: DashMap<(PathBuf, SystemTime), CacheEntry>,
    parsed: DashMap<(PathBuf, SystemTime), Arc<ParsedFile>>,
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
    /// Uses `entry()` API to avoid TOCTOU race between get and insert.
    pub fn get_or_compute(
        &self,
        path: &Path,
        mtime: SystemTime,
        compute: impl FnOnce() -> String,
    ) -> Arc<str> {
        match self.entries.entry((path.to_path_buf(), mtime)) {
            Entry::Occupied(e) => Arc::clone(&e.get().outline),
            Entry::Vacant(e) => {
                let outline: Arc<str> = compute().into();
                e.insert(CacheEntry {
                    outline: Arc::clone(&outline),
                });
                outline
            }
        }
    }

    /// Parse a code file with tree-sitter and cache the result. Returns
    /// `None` for non-code files, files larger than the 500 KB cap, or parse
    /// failures.
    #[must_use]
    pub fn get_or_parse(&self, path: &Path) -> Option<Arc<ParsedFile>> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta.modified().ok()?;
        if meta.len() > 500_000 {
            return None;
        }
        match self.parsed.entry((path.to_path_buf(), mtime)) {
            Entry::Occupied(e) => Some(Arc::clone(e.get())),
            Entry::Vacant(e) => {
                let crate::types::FileType::Code(lang) = crate::lang::detect_file_type(path) else {
                    return None;
                };
                let ts_lang = crate::lang::outline::outline_language(lang)?;
                let content = std::fs::read_to_string(path).ok()?;
                let mut parser = tree_sitter::Parser::new();
                parser.set_language(&ts_lang).ok()?;
                let tree = parser.parse(&content, None)?;
                let parsed = Arc::new(ParsedFile {
                    content: Arc::new(content),
                    tree,
                    lang,
                });
                e.insert(Arc::clone(&parsed));
                Some(parsed)
            }
        }
    }
}
