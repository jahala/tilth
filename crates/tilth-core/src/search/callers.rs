//! Structural call-site detection: every place a target symbol is called,
//! found with one parallel walk, a bloom and byte pre-filter, and a
//! tree-sitter pass over the files that survive.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use streaming_iterator::StreamingIterator;

use crate::error::TilthError;
use crate::lang::detect_file_type_with_content;
use crate::lang::outline::outline_language;
use crate::types::FileType;

/// Stop the batch caller walk once we have this many raw matches. Generous headroom for dedup + ranking.
pub const BATCH_EARLY_QUIT: usize = 50;

/// A single caller match — a call site of a target symbol.
#[derive(Debug)]
pub struct CallerMatch {
    pub path: PathBuf,
    pub line: u32,
    pub calling_function: String,
    pub call_text: String,
    /// Line range of the calling function (for expand).
    pub caller_range: Option<(u32, u32)>,
    /// The call sits in test code that only the source says is test code: a
    /// Rust `#[test]` function, a `#[cfg(test)]` module, or a file under a
    /// crate's `tests/` directory. `is_test_file` sees only path conventions.
    pub in_test: bool,
    /// File content, already read during `find_callers_batch` — avoids re-reading during expand.
    /// Shared across all call sites in the same file via reference counting.
    pub content: Arc<String>,
}

/// Find all call sites of any symbol in `targets` across the codebase using a single walk.
/// Returns tuples of (`target_name`, match) so callers know which symbol was matched.
// The std hasher is part of the signature dependents pin; a generic hasher
// would change it for every caller.
#[allow(clippy::implicit_hasher)]
pub fn find_callers_batch(
    targets: &HashSet<String>,
    scope: &Path,
    bloom: &crate::index::bloom::BloomFilterCache,
    glob: Option<&str>,
    early_quit_threshold: usize,
) -> Result<Vec<(String, CallerMatch)>, TilthError> {
    let matches: Mutex<Vec<(String, CallerMatch)>> = Mutex::new(Vec::new());
    let found_count = AtomicUsize::new(0);

    let walker = super::walk::walker(scope, glob)?;

    walker.run(|| {
        let matches = &matches;
        let found_count = &found_count;

        Box::new(move |entry| {
            // Early termination: enough callers found
            if found_count.load(Ordering::Relaxed) >= early_quit_threshold {
                return ignore::WalkState::Quit;
            }

            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };

            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }

            let path = entry.path();

            // Read + size-gate + bloom prefilter in one shared step.
            let Some((content, _mtime)) = super::bloom_walk::read_with_bloom_check(
                path,
                targets,
                bloom,
                super::bloom_walk::MAX_FILE_SIZE,
            ) else {
                return ignore::WalkState::Continue;
            };

            // Fast byte check via memchr::memmem (SIMD) — cheap second pass that
            // eliminates bloom false positives before tree-sitter parses.
            if !targets
                .iter()
                .any(|t| memchr::memmem::find(content.as_bytes(), t.as_bytes()).is_some())
            {
                return ignore::WalkState::Continue;
            }

            // Only process files with tree-sitter grammars
            let file_type = detect_file_type_with_content(path, &content);
            let FileType::Code(lang) = file_type else {
                return ignore::WalkState::Continue;
            };

            let Some(ts_lang) = outline_language(lang) else {
                return ignore::WalkState::Continue;
            };

            let file_callers =
                find_callers_treesitter_batch(path, scope, targets, &ts_lang, &content, lang);

            if !file_callers.is_empty() {
                found_count.fetch_add(file_callers.len(), Ordering::Relaxed);
                let mut all = matches
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                all.extend(file_callers);
            }

            ignore::WalkState::Continue
        })
    });

    Ok(matches
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
}

/// Tree-sitter call site detection for a set of target symbols.
/// Returns tuples of (`matched_target_name`, `CallerMatch`).
fn find_callers_treesitter_batch(
    path: &Path,
    scope: &Path,
    targets: &HashSet<String>,
    ts_lang: &tree_sitter::Language,
    content: &str,
    lang: crate::types::Lang,
) -> Vec<(String, CallerMatch)> {
    // Get the query string for this language
    let Some(query_str) = super::callee_query::callee_query_str(lang) else {
        return Vec::new();
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(ts_lang).is_err() {
        return Vec::new();
    }

    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };

    let content_bytes = content.as_bytes();
    let lines: Vec<&str> = content.lines().collect();

    // One Arc per file — all call sites share the same allocation.
    let shared_content: Arc<String> = Arc::new(content.to_string());

    let callee_filter = crate::lang::spec::spec(lang).callee_filter;
    // Test code the path alone does not reveal (see `test_site`).
    let test_site = crate::lang::spec::spec(lang).test_site;

    let Some(callers) = super::callee_query::with_callee_query(ts_lang, query_str, |query| {
        let Some(callee_idx) = query.capture_index_for_name("callee") else {
            return Vec::new();
        };

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), content_bytes);
        let mut callers = Vec::new();

        while let Some(m) = matches.next() {
            for cap in m.captures() {
                if cap.index != callee_idx {
                    continue;
                }

                // Check if the captured text matches any of our target symbols
                let Ok(text) = cap.node.utf8_text(content_bytes) else {
                    continue;
                };

                if !targets.contains(text) {
                    continue;
                }

                // Some patterns match more than a call (see `callee_filter`).
                if callee_filter.is_some_and(|keep| !keep(&cap.node)) {
                    continue;
                }

                let matched_target = text.to_string();

                // Found a call site! Now walk up to find the calling function
                let line = cap.node.start_position().row as u32 + 1;

                // Get the call text (the whole call expression, not just the
                // callee). When the parent spans lines no single line is the
                // call, so show the callee's own line instead of the bare name.
                // Applies to every language; a macro's token tree makes it the
                // common case for Rust.
                let call_node = cap.node.parent().unwrap_or(cap.node);
                let same_line = call_node.start_position().row == call_node.end_position().row;
                let row = if same_line {
                    call_node.start_position().row
                } else {
                    cap.node.start_position().row
                };
                let call_text: String = lines
                    .get(row)
                    .map_or_else(|| matched_target.clone(), |l| l.trim().to_string());

                // Walk up the tree to find the enclosing function
                let (calling_function, caller_range) =
                    find_enclosing_function(cap.node, &lines, lang);

                callers.push((
                    matched_target,
                    CallerMatch {
                        path: path.to_path_buf(),
                        line,
                        calling_function,
                        call_text,
                        caller_range,
                        in_test: test_site
                            .is_some_and(|in_test| in_test(&cap.node, content_bytes, path, scope)),
                        content: Arc::clone(&shared_content),
                    },
                ));
            }
        }

        callers
    }) else {
        return Vec::new();
    };

    callers
}

/// Walk up the AST from a node to find the enclosing function definition.
/// Returns (`function_name`, `line_range`). Top-level renders as `"<top-level>"`.
fn find_enclosing_function(
    node: tree_sitter::Node,
    lines: &[&str],
    lang: crate::types::Lang,
) -> (String, Option<(u32, u32)>) {
    match super::scope::walk_to_enclosing_definition(node, lines, lang) {
        Some((_, name, range)) => (name, Some(range)),
        None => ("<top-level>".to_string(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bloom pre-filter must never hide a file that holds a call site. A
    /// quote inside a triple-quoted string, or an apostrophe in a `#` comment,
    /// once made the scanner read the rest of the file as a string, so the
    /// names after it never reached the filter and their call sites vanished.
    #[test]
    fn call_sites_after_a_triple_quoted_string_or_hash_comment_are_found() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.py"), "def helper_after():\n    pass\n").unwrap();
        std::fs::write(
            dir.path().join("uses_docstring.py"),
            "FIXTURE = \"\"\"\n{\"prompt\": \"count ripgrep's lines\"}\n\"\"\"\n\n\
             def docstring_caller():\n    helper_after()\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("uses_comment.py"),
            "# don't call this twice\ndef comment_caller():\n    helper_after()\n",
        )
        .unwrap();

        let bloom = crate::index::bloom::BloomFilterCache::new();
        let targets: HashSet<String> = std::iter::once("helper_after".to_string()).collect();
        let found =
            find_callers_batch(&targets, dir.path(), &bloom, None, BATCH_EARLY_QUIT).unwrap();
        let mut callers: Vec<&str> = found
            .iter()
            .map(|(_, m)| m.calling_function.as_str())
            .collect();
        callers.sort_unstable();
        assert_eq!(callers, ["comment_caller", "docstring_caller"]);
    }
}
