//! The walker every relational query shares, and the structural queries
//! themselves: symbols, callers, callees, dependencies, blast radius.

pub mod blast;
pub mod bloom_walk;
pub mod callees;
pub mod callers;
pub mod deps;
pub mod facets;
pub mod rank;
pub mod scope;
pub mod siblings;
pub mod symbol;

mod callee_query;

use std::path::Path;
use std::time::SystemTime;

use ignore::WalkBuilder;

use crate::error::TilthError;

// Directories that are always skipped — build artifacts, dependencies, VCS internals.
// We skip these explicitly instead of relying on .gitignore so that locally-relevant
// gitignored files (docs/, configs, generated code) are still searchable.
pub const SKIP_DIRS: &[&str] = &[
    ".git",
    ".jj",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "dist",
    "build",
    "__pycache__",
    ".pycache",
    "vendor",
    ".next",
    ".nuxt",
    "coverage",
    ".cache",
    ".tox",
    ".venv",
    ".eggs",
    ".mypy_cache",
    ".ruff_cache",
    ".pytest_cache",
    ".turbo",
    ".parcel-cache",
    ".svelte-kit",
    "out",
    ".output",
    ".vercel",
    ".netlify",
    ".gradle",
    ".idea",
    ".scala-build",
    ".bloop",
    ".metals",
];

/// Shared walker policy: searches ALL files except known junk directories.
/// Does NOT respect .gitignore — ensures gitignored but locally-relevant files
/// are found. Used by both the parallel search walker (`walker()`) and the
/// sequential map walker (`crate::map::generate`), which each apply their own
/// final `.max_depth()`/`.threads()` and `.build()`/`.build_parallel()`.
#[must_use]
pub fn base_walk_builder(scope: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(scope);
    builder
        .follow_links(true)
        .same_file_system(true) // Stop at mount boundaries (NFS, external volumes).
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .ignore(false)
        .parents(false)
        .filter_entry(|entry| {
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                if let Some(name) = entry.file_name().to_str() {
                    return !SKIP_DIRS.contains(&name);
                }
            }
            true
        });
    builder
}

/// Build a parallel directory walker that searches ALL files except known junk directories.
/// Does NOT respect .gitignore — ensures gitignored but locally-relevant files are found.
/// When `glob` is Some, applies a file-pattern override (whitelist or negation).
pub fn walker(scope: &Path, glob: Option<&str>) -> Result<ignore::WalkParallel, TilthError> {
    let threads = std::env::var("TILTH_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).clamp(2, 6))
        });

    let mut builder = base_walk_builder(scope);
    builder.threads(threads);

    if let Some(pattern) = glob {
        if !pattern.is_empty() {
            let mut overrides = ignore::overrides::OverrideBuilder::new(scope);
            overrides
                .add(pattern)
                .map_err(|e| TilthError::InvalidQuery {
                    query: pattern.to_string(),
                    reason: format!("invalid glob: {e}"),
                })?;
            builder.overrides(overrides.build().map_err(|e| TilthError::InvalidQuery {
                query: pattern.to_string(),
                reason: format!("invalid glob: {e}"),
            })?);
        }
    }

    Ok(builder.build_parallel())
}

/// Get `file_lines` estimate and mtime from metadata. One `stat()` per file.
#[must_use]
pub fn file_metadata(path: &Path) -> (u32, SystemTime) {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let est_lines = (meta.len() / 40).max(1) as u32;
            (est_lines, mtime)
        }
        Err(_) => (0, SystemTime::UNIX_EPOCH),
    }
}

/// Format a token count into a human-readable string (e.g. "~1.2k" or "~743").
#[must_use]
pub fn format_token_count(tokens: u64) -> String {
    if tokens >= 1000 {
        format!("~{}.{}k", tokens / 1000, (tokens % 1000) / 100)
    } else {
        format!("~{tokens}")
    }
}
