use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use super::file_metadata;

use crate::error::TilthError;
use crate::search::rank;
use crate::types::{FacetTotals, Match, SearchResult};
use grep_regex::RegexMatcher;
use grep_searcher::sinks::UTF8;
use grep_searcher::Searcher;

const MAX_MATCHES: usize = 10;
const EARLY_QUIT_THRESHOLD: usize = MAX_MATCHES * 3;

/// Content search using ripgrep crates. Literal by default, regex if `is_regex`.
pub fn search(
    pattern: &str,
    scope: &Path,
    is_regex: bool,
    glob: Option<&str>,
) -> Result<SearchResult, TilthError> {
    let matcher = if is_regex {
        RegexMatcher::new(pattern)
    } else {
        RegexMatcher::new(&regex_syntax::escape(pattern))
    }
    .map_err(|e| TilthError::InvalidQuery {
        query: pattern.to_string(),
        reason: e.to_string(),
    })?;

    let matches: Mutex<Vec<Match>> = Mutex::new(Vec::new());
    // Relaxed is correct: walker.run() joins all threads before we read the final value.
    // Early-quit checks are approximate by design — one extra iteration is harmless.
    let total_found = AtomicUsize::new(0);

    let walker = super::walker(scope, glob)?;

    walker.run(|| {
        let matcher = &matcher;
        let matches = &matches;
        let total_found = &total_found;

        Box::new(move |entry| {
            if total_found.load(Ordering::Relaxed) >= EARLY_QUIT_THRESHOLD {
                return ignore::WalkState::Quit;
            }

            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };

            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }

            let path = entry.path();

            // Shared stat-only filter (minified-by-name + size cap). The
            // empty-header walker calls the same helper so the `Files
            // searched: N` count reported on zero-result responses always
            // matches the rule the real search applies.
            if !super::passes_stat_filter(path) {
                return ignore::WalkState::Continue;
            }
            let file_size = std::fs::metadata(path).map_or(0, |m| m.len());

            // Read the file once. Use `search_slice` instead of `search_path`
            // so the minified-check (when triggered) and the actual search
            // share a single kernel read — no double I/O, no TOCTOU window
            // between the heuristic and the search.
            let Ok(bytes) = std::fs::read(path) else {
                return ignore::WalkState::Continue;
            };

            // Catch unmarked minified bundles in the 100KB–500KB range. This
            // byte-content check is intentionally NOT part of
            // `passes_stat_filter`: it requires the file bytes, so the
            // empty-header walker cannot replicate it without paying the
            // read cost on every file. A non-empty real search reads the
            // bytes anyway and pays the extra check once per file.
            if file_size >= crate::lang::detection::MINIFIED_CHECK_THRESHOLD
                && crate::lang::detection::is_minified_by_content(&bytes)
            {
                return ignore::WalkState::Continue;
            }

            let (file_lines, mtime) = file_metadata(path);

            let mut file_matches = Vec::new();
            let mut searcher = Searcher::new();

            let _ = searcher.search_slice(
                matcher,
                &bytes,
                UTF8(|line_num, line| {
                    file_matches.push(Match {
                        path: path.to_path_buf(),
                        line: line_num as u32,
                        text: line.trim_end().to_string(),
                        is_definition: false,
                        exact: false,
                        file_lines,
                        mtime,
                        def_range: None,
                        def_name: None,
                        def_weight: 0,
                        impl_target: None,
                    });
                    Ok(true)
                }),
            );

            if !file_matches.is_empty() {
                total_found.fetch_add(file_matches.len(), Ordering::Relaxed);
                let mut all = matches
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                all.extend(file_matches);
            }

            if total_found.load(Ordering::Relaxed) >= EARLY_QUIT_THRESHOLD {
                ignore::WalkState::Quit
            } else {
                ignore::WalkState::Continue
            }
        })
    });

    let total = total_found.load(Ordering::Relaxed);
    let mut all_matches = matches
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    rank::sort(&mut all_matches, pattern, scope);
    all_matches.truncate(MAX_MATCHES);

    Ok(SearchResult {
        query: pattern.to_string(),
        scope: scope.to_path_buf(),
        matches: all_matches,
        total_found: total,
        definitions: 0,
        usages: total,
        facet_totals: FacetTotals::default(),
    })
}
