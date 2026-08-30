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

/// Glob search using `ignore::WalkBuilder` (parallel via `super::walker` —
/// deliberately NOT .gitignore-aware, see `walker`'s doc comment).
pub fn search(pattern: &str, scope: &Path) -> Result<GlobResult, TilthError> {
    let glob = Glob::new(pattern).map_err(|e| TilthError::InvalidQuery {
        query: pattern.to_string(),
        reason: e.to_string(),
    })?;
    let matcher = glob.compile_matcher();

    let matched: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
    let extensions: std::sync::Mutex<HashSet<String>> = std::sync::Mutex::new(HashSet::new());

    let walker = super::walker(scope, None)?;

    walker.run(|| {
        let matcher = &matcher;
        let matched = &matched;
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
                matched
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(path.to_path_buf());
            }

            ignore::WalkState::Continue
        })
    });

    let mut matched = matched
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let extensions = extensions
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let total = matched.len();

    // WalkParallel visits files in nondeterministic order; sort before capping
    // so the selected subset is stable. Previews are computed only for survivors.
    matched.sort();
    matched.truncate(MAX_FILES);
    let files: Vec<GlobFileEntry> = matched
        .into_iter()
        .map(|path| {
            let preview = file_preview(&path);
            GlobFileEntry { path, preview }
        })
        .collect();

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_selection_is_deterministic_and_sorted() {
        // More matches than MAX_FILES so the cap engages.
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..(MAX_FILES + 15) {
            std::fs::write(tmp.path().join(format!("f{i:03}.rs")), "fn main() {}").unwrap();
        }

        let first: Vec<PathBuf> = search("*.rs", tmp.path())
            .unwrap()
            .files
            .into_iter()
            .map(|f| f.path)
            .collect();
        let second: Vec<PathBuf> = search("*.rs", tmp.path())
            .unwrap()
            .files
            .into_iter()
            .map(|f| f.path)
            .collect();

        // The cap must select the lexicographically smallest matches, sorted —
        // the same set every run, regardless of parallel walk order.
        let expected: Vec<PathBuf> = (0..MAX_FILES)
            .map(|i| tmp.path().join(format!("f{i:03}.rs")))
            .collect();
        assert_eq!(
            first, expected,
            "capped set must be deterministic and sorted"
        );
        assert_eq!(first, second, "two identical runs must return the same set");
    }

    #[test]
    fn glob_total_found_counts_every_match() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..(MAX_FILES + 5) {
            std::fs::write(tmp.path().join(format!("g{i:03}.rs")), "fn main() {}").unwrap();
        }
        let result = search("*.rs", tmp.path()).unwrap();
        assert_eq!(result.files.len(), MAX_FILES, "file list stays capped");
        assert_eq!(
            result.total_found,
            MAX_FILES + 5,
            "total_found reports the full match count"
        );
    }
}
