use std::collections::HashSet;
use std::path::{Path, PathBuf};

use globset::Glob;

use crate::error::TilthError;
use crate::types::estimate_tokens;

const MAX_FILES: usize = 20;

pub struct GlobFileEntry {
    pub path: PathBuf,
    pub preview: Option<String>,
}

pub struct GlobResult {
    pub pattern: String,
    pub files: Vec<GlobFileEntry>,
    pub total_found: usize,
    pub available_extensions: Vec<String>,
}

/// Glob search using `ignore::WalkBuilder` (parallel, .gitignore-aware).
pub fn search(pattern: &str, scope: &Path) -> Result<GlobResult, TilthError> {
    let glob = Glob::new(pattern).map_err(|e| TilthError::InvalidQuery {
        query: pattern.to_string(),
        reason: e.to_string(),
    })?;
    let matcher = glob.compile_matcher();

    let files: std::sync::Mutex<Vec<GlobFileEntry>> = std::sync::Mutex::new(Vec::new());
    let total_found = std::sync::atomic::AtomicUsize::new(0);
    let extensions: std::sync::Mutex<HashSet<String>> = std::sync::Mutex::new(HashSet::new());

    let walker = super::walker(scope, None)?;

    walker.run(|| {
        let matcher = &matcher;
        let files = &files;
        let total_found = &total_found;
        let extensions = &extensions;

        Box::new(move |entry| {
            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };

            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }

            let path = entry.path();

            // Collect extensions for zero-match suggestions
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                extensions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(ext.to_string());
            }

            // Match against filename or relative path
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let rel = path.strip_prefix(scope).unwrap_or(path);

            if matcher.is_match(name) || matcher.is_match(rel) {
                total_found.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // Compute preview outside the lock, then check-and-push in one acquisition
                let preview = file_preview(path);
                let mut locked = files
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if locked.len() < MAX_FILES {
                    locked.push(GlobFileEntry {
                        path: path.to_path_buf(),
                        preview,
                    });
                }
            }

            ignore::WalkState::Continue
        })
    });

    let total = total_found.load(std::sync::atomic::Ordering::Relaxed);
    let files = files
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let extensions = extensions
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let available_extensions: Vec<String> = if files.is_empty() {
        let mut exts: Vec<String> = extensions.into_iter().collect();
        exts.sort();
        exts.truncate(10);
        exts
    } else {
        Vec::new()
    };

    Ok(GlobResult {
        pattern: pattern.to_string(),
        files,
        total_found: total,
        available_extensions,
    })
}

/// Quick preview: token estimate, or "test file", or "module" based on exports.
fn file_preview(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let tokens = estimate_tokens(meta.len());
    Some(format!("~{tokens} tokens"))
}
