//! Fuzzy path resolution for missing path-like queries.
//!
//! When an agent hands tilth a slightly-off path (wrong directory component,
//! missing prefix, basename-only), this resolves it to the best-matching real
//! file(s) via subsequence path matching (`nucleo-matcher` in `match_paths`
//! mode) and returns them as a ranked "did you mean" suggestion list — it never
//! opens a file the agent didn't name. tilth's value is being a precise layer
//! an agent doesn't second-guess; auto-opening a different file than was asked
//! for is a confidently-wrong failure mode, and the round-trip a suggestion
//! costs is cheap by comparison.
//!
//! Cold path only — never invoked on a successful read or search. The walk is
//! pruned by `.tilthignore` (not `.gitignore`), includes hidden and gitignored
//! files, and follows symlinks — but only path names ever surface as
//! suggestions, never file contents.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Upper bound on files scored. The cold-path whole-tree walk stops here and
/// the truncation is logged — never silently capped (project rule).
const MAX_FUZZY_CANDIDATES: usize = 20_000;

/// How many candidates to return as "did you mean" suggestions.
const SUGGESTION_K: usize = 3;

/// Outcome of resolving a missing path-like query.
pub enum FuzzyResolution {
    /// Ambiguous or low-confidence — feed a "did you mean" list.
    Suggestions(Vec<String>),
    /// No subsequence candidate — caller keeps the unchanged `NotFound`.
    None,
}

/// Tuning profile per call site. Retained so callers document which miss they
/// came from (a plain read vs. a search fallback); suggest-only ranks and caps
/// identically for both today, but the profile is where a future ranking
/// distinction would hang.
#[derive(Clone, Copy)]
pub enum GateProfile {
    Read,
    Search,
}

/// Resolve `query` against the `.tilthignore`-pruned file tree rooted at `scope`.
///
/// Walks `search::walker(scope, None)`, scores every file's scope-relative path
/// against `query` with `nucleo-matcher`'s path-aware matcher, and returns the
/// top [`SUGGESTION_K`] as a ranked "did you mean" list. nucleo returns `None`
/// unless `query` is a subsequence of the candidate — that hard filter rejects
/// stale/garbage paths (they never surface).
#[must_use]
pub fn resolve_fuzzy_path(scope: &Path, query: &str, _gate: GateProfile) -> FuzzyResolution {
    let (candidates, truncated) = collect_candidates(scope);
    if truncated {
        eprintln!(
            "tilth: fuzzy path resolution scored only the first {MAX_FUZZY_CANDIDATES} files \
             (tree larger than cap) for query {query:?} — result may miss a better match"
        );
    }

    let mut scored = score_candidates(query, &candidates);
    if scored.is_empty() {
        return FuzzyResolution::None;
    }
    // Highest score first; stable tie order keeps equal-score candidates as the
    // walker yielded them (sorted by path in `collect_candidates`).
    scored.sort_by_key(|&(score, _)| std::cmp::Reverse(score));

    let suggestions = scored
        .into_iter()
        .take(SUGGESTION_K)
        .map(|(_, p)| p.to_string_lossy().into_owned())
        .collect();
    FuzzyResolution::Suggestions(suggestions)
}

/// Search-miss suggestions for the MCP `tilth_search` default path, which
/// returns an empty-result header on a miss and so never reaches the basic-path
/// `fuzzy_path_fallback`. Non-path-like queries return `None` before any walk
/// (guarded here via [`is_path_like`], so a normal empty symbol search never
/// walks the tree); callers confirm the search produced no matches before
/// invoking this. Returns the ranked "did you mean" list for a path-like miss,
/// or `None` when nothing subsequence-matches (the caller keeps its own
/// empty-result output unchanged). Never opens a file the agent didn't name.
#[must_use]
pub fn search_miss_suggestions(scope: &Path, query: &str) -> Option<Vec<String>> {
    if !is_path_like(query) {
        return None;
    }
    match resolve_fuzzy_path(scope, query, GateProfile::Search) {
        FuzzyResolution::Suggestions(s) => Some(s),
        FuzzyResolution::None => None,
    }
}

/// nucleo path-matcher over collected scope-relative paths. Returns
/// `(score, relative_path)` for every subsequence match. The `PathBuf` is
/// built only for the handful of matches — non-matching candidates stay
/// `String`-only from the walk.
fn score_candidates(query: &str, candidates: &[String]) -> Vec<(u16, PathBuf)> {
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let atom = Atom::new(
        query,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
        false,
    );
    let mut buf: Vec<char> = Vec::new();
    candidates
        .iter()
        .filter_map(|rel_str| {
            let haystack = Utf32Str::new(rel_str, &mut buf);
            atom.score(haystack, &mut matcher)
                .map(|score| (score, PathBuf::from(rel_str)))
        })
        .collect()
}

/// A query is path-like when it contains a path separator and the final
/// segment has a file extension. The MCP search-miss path uses this as a
/// pre-check so a normal bare-concept search never walks the whole tree.
pub fn is_path_like(query: &str) -> bool {
    query.contains('/') && Path::new(query).extension().is_some()
}

/// Walk the `.tilthignore`-pruned tree under `scope`, collecting scope-relative
/// path strings for files only. Returns `(candidates, truncated)`; `truncated`
/// is true when the walk stopped at `MAX_FUZZY_CANDIDATES`.
fn collect_candidates(scope: &Path) -> (Vec<String>, bool) {
    let Ok(walker) = crate::search::walker(scope, None) else {
        return (Vec::new(), false);
    };

    let collected: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let count = AtomicUsize::new(0);
    let truncated = AtomicBool::new(false);

    walker.run(|| {
        let collected = &collected;
        let count = &count;
        let truncated = &truncated;
        Box::new(move |entry| {
            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }
            if count.fetch_add(1, Ordering::Relaxed) >= MAX_FUZZY_CANDIDATES {
                truncated.store(true, Ordering::Relaxed);
                return ignore::WalkState::Quit;
            }
            let path = entry.path();
            let rel = path.strip_prefix(scope).unwrap_or(path);
            let rel_str = rel.to_string_lossy().into_owned();
            collected
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(rel_str);
            ignore::WalkState::Continue
        })
    });

    let mut v = collected
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Deterministic order so equal-score ties resolve stably (and tests are
    // reproducible regardless of walk thread interleaving).
    v.sort();
    (v, truncated.load(Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fixture tree under a fresh temp dir from a list of relative paths.
    fn fixture(paths: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for rel in paths {
            let p = dir.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "fixture\n").unwrap();
        }
        dir
    }

    /// The top-ranked (first) suggestion — what a `Resolved` auto-open would
    /// have opened, pre-suggest-only. Suggest-only always returns this as
    /// `Suggestions[0]` rather than opening it silently.
    fn top_suggestion(res: &FuzzyResolution) -> Option<String> {
        match res {
            FuzzyResolution::Suggestions(s) => s.first().cloned(),
            FuzzyResolution::None => None,
        }
    }

    #[test]
    fn wrong_dir_missing_prefix_suggests_real_file_first() {
        // `_mcp_core.py` given, real file lives under src/milknado/.
        let dir = fixture(&[
            "src/milknado/_mcp_core.py",
            "src/other/util.py",
            "README.md",
        ]);
        let res = resolve_fuzzy_path(dir.path(), "_mcp_core.py", GateProfile::Read);
        assert_eq!(
            top_suggestion(&res).as_deref(),
            Some("src/milknado/_mcp_core.py"),
            "wrong-dir/missing-prefix query should suggest the real file first"
        );
    }

    #[test]
    fn partial_basename_suggests_single_match_first() {
        let dir = fixture(&["src/search/symbol.rs", "src/lib.rs", "src/read/mod.rs"]);
        let res = resolve_fuzzy_path(dir.path(), "symbol.rs", GateProfile::Read);
        assert_eq!(
            top_suggestion(&res).as_deref(),
            Some("src/search/symbol.rs"),
            "basename-only query should suggest the single match first"
        );
    }

    #[test]
    fn deletion_typo_suggests_real_file_first() {
        // `serch` is missing the `a` from `search` — still a subsequence.
        let dir = fixture(&["src/search/symbol.rs", "src/lib.rs"]);
        let res = resolve_fuzzy_path(dir.path(), "serch/symbol.rs", GateProfile::Read);
        assert_eq!(
            top_suggestion(&res).as_deref(),
            Some("src/search/symbol.rs"),
            "deletion typo should still subsequence-match and suggest first"
        );
    }

    #[test]
    fn ambiguous_basename_suggests_both_candidates_read() {
        let dir = fixture(&["src/a/mod.rs", "src/b/mod.rs"]);
        let res = resolve_fuzzy_path(dir.path(), "mod.rs", GateProfile::Read);
        match res {
            FuzzyResolution::Suggestions(s) => {
                assert!(s.len() >= 2, "expected both mod.rs candidates: {s:?}");
                assert!(s.iter().any(|p| p.contains('a')) && s.iter().any(|p| p.contains('b')));
            }
            other @ FuzzyResolution::None => {
                panic!("expected Suggestions for ambiguous basename, got {other:?}")
            }
        }
    }

    #[test]
    fn ambiguous_basename_suggests_both_candidates_search() {
        // A path-like query that is an equally-good subsequence of two files.
        // Suggest-only never auto-opens regardless of profile, so both
        // candidates should surface as suggestions.
        let dir = fixture(&["pkg1/a/mod.rs", "pkg2/a/mod.rs"]);
        let res = resolve_fuzzy_path(dir.path(), "a/mod.rs", GateProfile::Search);
        match res {
            FuzzyResolution::Suggestions(s) => {
                assert!(s.len() >= 2, "expected both a/mod.rs candidates: {s:?}");
            }
            other @ FuzzyResolution::None => {
                panic!("expected Suggestions for ambiguous basename, got {other:?}")
            }
        }
    }

    #[test]
    fn suggestions_capped_at_k() {
        // Five equally-good `x.rs` matches must collapse to exactly SUGGESTION_K.
        let dir = fixture(&["a/x.rs", "b/x.rs", "c/x.rs", "d/x.rs", "e/x.rs"]);
        let res = resolve_fuzzy_path(dir.path(), "x.rs", GateProfile::Read);
        match res {
            FuzzyResolution::Suggestions(s) => assert_eq!(
                s.len(),
                SUGGESTION_K,
                "ambiguous candidates must be capped at k={SUGGESTION_K}, got {s:?}"
            ),
            other @ FuzzyResolution::None => {
                panic!("expected Suggestions for 5-way tie, got {other:?}")
            }
        }
    }

    #[test]
    fn garbage_path_returns_none() {
        let dir = fixture(&["src/search/symbol.rs", "src/lib.rs"]);
        let res = resolve_fuzzy_path(dir.path(), "zzqq/nonexistent_xyzzy.bin", GateProfile::Read);
        assert!(
            matches!(res, FuzzyResolution::None),
            "a non-subsequence garbage path must stay an unchanged NotFound"
        );
    }

    #[test]
    fn search_profile_suggests_non_path_like_query() {
        // `symbol` is a subsequence of src/search/symbol.rs even though it has
        // no separator/extension — suggest-only surfaces it as a suggestion
        // regardless of path-likeness (that gate only ever governed auto-open).
        let dir = fixture(&["src/search/symbol.rs"]);
        let res = resolve_fuzzy_path(dir.path(), "symbol", GateProfile::Search);
        assert!(
            matches!(res, FuzzyResolution::Suggestions(_)),
            "non-path-like query with a candidate should suggest: {res:?}"
        );
    }

    #[test]
    fn search_profile_suggests_path_like_single_winner() {
        let dir = fixture(&["src/search/symbol.rs", "src/lib.rs", "README.md"]);
        let res = resolve_fuzzy_path(dir.path(), "serch/symbol.rs", GateProfile::Search);
        assert_eq!(
            top_suggestion(&res).as_deref(),
            Some("src/search/symbol.rs"),
            "path-like single-winner query should suggest the real file first under Search"
        );
    }

    #[test]
    fn search_miss_suggestions_guards_non_path_like() {
        // `symbol` subsequence-matches src/search/symbol.rs, but a bare-concept
        // query must return None before any tree walk — the internal
        // `is_path_like` guard keeps the pub API safe without relying on
        // caller pre-checks.
        let dir = fixture(&["src/search/symbol.rs"]);
        assert!(
            search_miss_suggestions(dir.path(), "symbol").is_none(),
            "non-path-like query must be guarded to None"
        );
        assert_eq!(
            search_miss_suggestions(dir.path(), "serch/symbol.rs")
                .unwrap()
                .first()
                .map(String::as_str),
            Some("src/search/symbol.rs"),
            "path-like miss must still return the did-you-mean list"
        );
    }

    impl std::fmt::Debug for FuzzyResolution {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                FuzzyResolution::Suggestions(s) => write!(f, "Suggestions({s:?})"),
                FuzzyResolution::None => write!(f, "None"),
            }
        }
    }
}
