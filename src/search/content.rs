use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use super::file_metadata;

use crate::search::rank;
use crate::types::{FacetTotals, Match, SearchResult};
use grep_regex::RegexMatcher;
use grep_searcher::sinks::UTF8;
use grep_searcher::Searcher;
use tilth_core::error::TilthError;

const MAX_MATCHES: usize = 10;
const FULL_MAX_MATCHES: usize = 100;

/// Content search using ripgrep crates. Literal by default, regex if `is_regex`.
pub fn search(
    pattern: &str,
    scope: &Path,
    is_regex: bool,
    context: Option<&Path>,
    glob: Option<&str>,
    full: bool,
) -> Result<SearchResult, TilthError> {
    let max_matches = if full { FULL_MAX_MATCHES } else { MAX_MATCHES };
    search_capped(
        pattern,
        scope,
        is_regex,
        context,
        glob,
        max_matches,
        max_matches,
    )
}

/// The display cap and the per-file collection bound are the same number in
/// production. They are separate parameters so a test can raise one without
/// the other and show that the bound leaves the displayed set untouched.
fn search_capped(
    pattern: &str,
    scope: &Path,
    is_regex: bool,
    context: Option<&Path>,
    glob: Option<&str>,
    max_matches: usize,
    per_file_cap: usize,
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
    let skipped_large: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    // Relaxed is correct: walker.run() joins all threads before we read the final value.
    let total_found = AtomicUsize::new(0);

    let walker = tilth_core::search::walk::walker(scope, glob)?;
    // One clock for the per-file bound and the final sort, so both rank by
    // the same scores.
    let now = std::time::SystemTime::now();

    walker.run(|| {
        let matcher = &matcher;
        let matches = &matches;
        let skipped_large = &skipped_large;
        let total_found = &total_found;

        Box::new(move |entry| {
            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };

            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }

            let path = entry.path();

            // Skip files that look minified by filename — `.min.js`, `app-min.css`.
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(tilth_core::lang::detection::is_minified_by_name)
            {
                return ignore::WalkState::Continue;
            }

            // Skip oversized files — tree-sitter and ripgrep shouldn't spend time on minified bundles
            let file_size = match std::fs::metadata(path) {
                Ok(meta) => {
                    if meta.len() > tilth_core::source::MAX_FILE_SIZE {
                        if super::oversized_holds(path, |b| super::has_match(matcher, b)) {
                            super::record(skipped_large, path);
                        }
                        return ignore::WalkState::Continue;
                    }
                    meta.len()
                }
                Err(_) => 0,
            };

            // Read the file once. Use `search_slice` instead of `search_path`
            // so the minified-check (when triggered) and the actual search
            // share a single kernel read — no double I/O, no TOCTOU window
            // between the heuristic and the search.
            let Ok(bytes) = std::fs::read(path) else {
                return ignore::WalkState::Continue;
            };
            let bytes = tilth_core::source::strip_bom_bytes(&bytes);

            // Catch unmarked minified bundles between 100 KB and the size cap.
            if file_size >= tilth_core::lang::detection::MINIFIED_CHECK_THRESHOLD
                && tilth_core::lang::detection::is_minified_by_content(bytes)
            {
                return ignore::WalkState::Continue;
            }

            let (file_lines, mtime) = file_metadata(path);

            let mut file_matches = Vec::new();
            let mut searcher = Searcher::new();

            let _ = searcher.search_slice(
                matcher,
                bytes,
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
                // Carry only this file's own top `per_file_cap` forward. The
                // global top-N is always a subset of the per-file top-Ns under
                // the same comparator, so nothing displayed changes, while the
                // collected set stays bounded by files × cap rather than by the
                // hit count — which is what the early quit used to bound.
                rank::retain_top_k(
                    &mut file_matches,
                    pattern,
                    scope,
                    context,
                    now,
                    per_file_cap,
                );
                let mut all = matches
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                all.extend(file_matches);
            }

            ignore::WalkState::Continue
        })
    });

    // The walk always completes, so `total` is the exact hit count and the
    // discovered set no longer depends on thread timing — quitting on a racy
    // shared counter made identical calls return different totals and silently
    // miss whole files. What bounds the work is the per-file cap above, which
    // is deterministic because it is decided inside one file.
    let total = total_found.load(Ordering::Relaxed);
    let mut all_matches = matches
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    rank::sort(&mut all_matches, pattern, scope, context, now);
    all_matches.truncate(max_matches);

    Ok(SearchResult {
        query: pattern.to_string(),
        scope: scope.to_path_buf(),
        matches: all_matches,
        total_found: total,
        definitions: 0,
        usages: total,
        facet_totals: FacetTotals::default(),
        skipped_large: super::sorted(skipped_large),
        other_owners: Vec::new(),
    })
}

#[cfg(test)]
mod completeness_tests {
    use super::*;
    use std::fmt::Write;

    /// One needle per file, in more files than the old shared-counter quit
    /// could ever have read: it stopped at 30 counted hits, and at the default
    /// thread count (clamped to 6) at most ~36 files were ever opened. A large
    /// `TILTH_THREADS` raises that ceiling, so the margin here is deliberate.
    const FILES: usize = 60;

    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for i in 0..FILES {
            std::fs::write(
                d.path().join(format!("f{i:02}.rs")),
                format!("fn f{i}() {{}}\n// needle_alpha marker line\n"),
            )
            .unwrap();
        }
        d
    }

    /// The walk must visit EVERY file before capping — quitting on a racy
    /// shared counter made the discovered SET thread-timing dependent:
    /// identical calls returned different totals and silently missed whole
    /// files while the result read as complete.
    #[test]
    fn content_search_totals_are_complete_and_deterministic() {
        let d = fixture();
        let r1 = search("needle_alpha", d.path(), false, None, None, false).unwrap();
        assert_eq!(
            r1.total_found, FILES,
            "must count the needle in ALL files, not quit mid-walk"
        );
        let first: Vec<_> = r1.matches.iter().map(|m| (&m.path, m.line)).collect();
        for _ in 0..4 {
            let r = search("needle_alpha", d.path(), false, None, None, false).unwrap();
            assert_eq!(
                r.total_found, r1.total_found,
                "totals must not vary run-to-run"
            );
            let shown: Vec<_> = r.matches.iter().map(|m| (&m.path, m.line)).collect();
            assert_eq!(
                shown, first,
                "the displayed match set must be identical run-to-run"
            );
        }
    }

    /// The per-file collection bound must be invisible in the result: at the
    /// production bound and at a raised one, the exact total and the displayed
    /// set are the same. Dropping the bound to one match per file shows it is
    /// live — and that the count is kept separately from the collection.
    #[test]
    fn per_file_cap_bounds_collection_without_changing_results() {
        let d = tempfile::tempdir().unwrap();
        let mut body = String::new();
        for i in 0..11 {
            let _ = writeln!(body, "// needle_gamma mentioned {i}");
        }
        // Highest-ranked line, and deliberately the LAST one: a "first k per
        // file" bound would drop exactly this match.
        body.push_str("pub fn needle_gamma() {}\n");
        std::fs::write(d.path().join("one.rs"), body).unwrap();

        let shown = |r: &SearchResult| -> Vec<(u32, String)> {
            r.matches.iter().map(|m| (m.line, m.text.clone())).collect()
        };
        let run =
            |cap| search_capped("needle_gamma", d.path(), false, None, None, 10, cap).unwrap();

        let bounded = run(10);
        let unbounded = run(usize::MAX);
        assert_eq!(
            bounded.total_found, 12,
            "the count must stay exact under the bound"
        );
        assert_eq!(unbounded.total_found, 12);
        assert_eq!(
            shown(&bounded),
            shown(&unbounded),
            "the bound must not change the displayed set"
        );
        assert_eq!(
            bounded.matches[0].line, 12,
            "the definition-shaped line outranks the comments above it"
        );

        let one_per_file = run(1);
        assert_eq!(
            one_per_file.matches.len(),
            1,
            "a per-file bound of 1 must bound what is collected"
        );
        assert_eq!(
            one_per_file.total_found, 12,
            "and the count must still be exact"
        );
    }
}
