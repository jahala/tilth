//! Fuzzy path resolution for missing path-like queries.
//!
//! When an agent hands tilth a slightly-off path (wrong directory component,
//! missing prefix, basename-only), this resolves it to the best-matching real
//! file via subsequence path matching (`nucleo-matcher` in `match_paths` mode)
//! so the caller can auto-open it instead of only emitting a "did you mean".
//!
//! Cold path only — never invoked on a successful read or search. The matcher
//! lives entirely behind [`resolve_fuzzy_path`]; the documented step-down (swap
//! to a no-dependency basename match) is a body swap, not a rewrite.
//! Walk honors gitignore rules, skips hidden files, and does not follow
//! symlinks — a misdirected query cannot auto-open secrets that were never
//! named by the agent.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::cache::OutlineCache;
use crate::error::TilthError;

// ── tuning constants ──────────────────────────────────────────────────────
// All gate thresholds live here. nucleo's `SCORE_MATCH` is 16 per matched
// char plus boundary bonuses, so a genuine path/basename match scores in the
// low hundreds while an incidental subsequence match scores far lower.

/// Upper bound on files scored. The cold-path whole-tree walk stops here and
/// the truncation is logged — never silently capped (project rule).
const MAX_FUZZY_CANDIDATES: usize = 20_000;

/// How many candidates to return as "did you mean" suggestions.
const SUGGESTION_K: usize = 3;

/// Read profile: lenient. Auto-open on a clear winner across the whole tree.
const READ_MIN_SCORE: u16 = 40;
const READ_MARGIN: f32 = 1.10;

/// Search profile: stricter. A file materializing from a *search* call is more
/// surprising, so demand a higher floor and a wider margin over the runner-up.
const SEARCH_MIN_SCORE: u16 = 80;
const SEARCH_MARGIN: f32 = 1.25;

/// A scored fuzzy candidate. `path` is scope-relative.
pub struct FuzzyHit {
    pub path: PathBuf,
    pub score: u16,
}

impl FuzzyHit {
    /// Emit an operator-log line recording an auto-open. Cold path, so the
    /// stderr trace is rare; it explains *why* a path the agent didn't ask for
    /// materialized, and surfaces the match `score` behind the decision.
    pub fn log_auto_open(&self, query: &str) {
        eprintln!(
            "tilth: fuzzy-resolved {query:?} → {:?} (score {})",
            self.path.display(),
            self.score
        );
    }
}

/// Outcome of resolving a missing path-like query.
pub enum FuzzyResolution {
    /// Gate passed — caller may auto-open this file.
    Resolved(FuzzyHit),
    /// Ambiguous or low-confidence — feed a "did you mean" list.
    Suggestions(Vec<String>),
    /// No subsequence candidate — caller keeps the unchanged `NotFound`.
    None,
}

/// Tuning profile per call site. `Read` is broad/lenient; `Search` is tight and
/// additionally requires the query to look path-like before it will auto-open.
#[derive(Clone, Copy)]
pub enum GateProfile {
    Read,
    Search,
}

/// Resolve `query` against the file tree rooted at `scope`, honoring
/// gitignore rules and skipping hidden files. Symlinks are not followed.
///
/// Walks the scope with a dedicated security-conservative walker (hidden files
/// excluded, .gitignore honored, symlinks not followed), scores every file's
/// scope-relative path against `query` with `nucleo-matcher`'s path-aware
/// matcher, and applies the confidence gate for `gate`. nucleo returns `None`
/// unless `query` is a subsequence of the candidate — that hard filter rejects
/// stale/garbage paths (they never auto-resolve).
#[must_use]
pub fn resolve_fuzzy_path(scope: &Path, query: &str, gate: GateProfile) -> FuzzyResolution {
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

    apply_gate(scored, gate, query)
}

/// Read a confidently-resolved search hit and prepend the distinct search
/// auto-open header. Shared by the basic-path fallback (`lib::fuzzy_path_fallback`)
/// and the expanded MCP path so the header text never drifts between the two.
pub fn search_auto_open_body(
    scope: &Path,
    hit: &FuzzyHit,
    query: &str,
    cache: &OutlineCache,
) -> Result<String, TilthError> {
    hit.log_auto_open(query);
    let real = scope.join(&hit.path);
    let body = super::read_file(&real, None, false, cache, false)?;
    Ok(format!(
        "# {} (resolved from path-like query \"{query}\") · no search matches; closest file auto-opened\n\n{body}",
        hit.path.display()
    ))
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

/// Apply the confidence gate to score-sorted candidates.
///
/// `Resolved` iff (for `Search`, the query is path-like AND) the top candidate
/// clears `MIN_SCORE` and either is unique or beats the runner-up by `MARGIN`.
/// Otherwise the top `SUGGESTION_K` become a "did you mean" list.
fn apply_gate(scored: Vec<(u16, PathBuf)>, gate: GateProfile, query: &str) -> FuzzyResolution {
    let (min_score, margin, require_path_like) = match gate {
        GateProfile::Read => (READ_MIN_SCORE, READ_MARGIN, false),
        GateProfile::Search => (SEARCH_MIN_SCORE, SEARCH_MARGIN, true),
    };

    let path_like_ok = !require_path_like || is_path_like(query);
    let (top_score, top_path) = &scored[0];
    let unique = scored.len() == 1;
    let clears_margin = unique || f32::from(*top_score) >= f32::from(scored[1].0) * margin;
    let clears_floor = *top_score >= min_score;

    if path_like_ok && clears_margin && clears_floor {
        return FuzzyResolution::Resolved(FuzzyHit {
            path: top_path.clone(),
            score: *top_score,
        });
    }

    let suggestions = scored
        .into_iter()
        .take(SUGGESTION_K)
        .map(|(_, p)| p.to_string_lossy().into_owned())
        .collect();
    FuzzyResolution::Suggestions(suggestions)
}

/// A query is path-like when it contains a path separator (`/` or `\\`).
/// An extension tightens the gate further but is not required — so
/// `config/Dockerfile` resolves while a bare `symbol` token does not.
/// Used by the `Search` gate to refuse auto-opening a bare-concept query
/// that merely happens to fuzzy-match a file.
fn is_path_like(query: &str) -> bool {
    query.contains('/') || query.contains('\\')
}

/// Walk the scope, honoring gitignore and skipping hidden files and symlinks,
/// collecting scope-relative path strings for files only. Uses a dedicated
/// walker (not the shared `search::walker`) so security settings cannot drift.
/// Returns `(candidates, truncated)`; `truncated` is true when the walk
/// stopped at `MAX_FUZZY_CANDIDATES`.
fn collect_candidates(scope: &Path) -> (Vec<String>, bool) {
    use ignore::WalkBuilder;
    let threads = std::env::var("TILTH_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).clamp(2, 6))
        });
    let walker = WalkBuilder::new(scope)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .ignore(true)
        .parents(true)
        .require_git(false)
        .follow_links(false)
        .threads(threads)
        .build_parallel();

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

    fn resolved_path(res: &FuzzyResolution) -> Option<String> {
        match res {
            FuzzyResolution::Resolved(hit) => Some(hit.path.to_string_lossy().into_owned()),
            _ => None,
        }
    }

    #[test]
    fn resolves_wrong_dir_missing_prefix() {
        // `_mcp_core.py` given, real file lives under src/milknado/.
        let dir = fixture(&[
            "src/milknado/_mcp_core.py",
            "src/other/util.py",
            "README.md",
        ]);
        let res = resolve_fuzzy_path(dir.path(), "_mcp_core.py", GateProfile::Read);
        assert_eq!(
            resolved_path(&res).as_deref(),
            Some("src/milknado/_mcp_core.py"),
            "wrong-dir/missing-prefix query should auto-resolve"
        );
    }

    #[test]
    fn resolves_partial_basename_under_read() {
        let dir = fixture(&["src/search/symbol.rs", "src/lib.rs", "src/read/mod.rs"]);
        let res = resolve_fuzzy_path(dir.path(), "symbol.rs", GateProfile::Read);
        assert_eq!(
            resolved_path(&res).as_deref(),
            Some("src/search/symbol.rs"),
            "basename-only query should resolve to the single match"
        );
    }

    #[test]
    fn resolves_deletion_typo() {
        // `serch` is missing the `a` from `search` — still a subsequence.
        let dir = fixture(&["src/search/symbol.rs", "src/lib.rs"]);
        let res = resolve_fuzzy_path(dir.path(), "serch/symbol.rs", GateProfile::Read);
        assert_eq!(
            resolved_path(&res).as_deref(),
            Some("src/search/symbol.rs"),
            "deletion typo should still subsequence-match"
        );
    }

    #[test]
    fn ambiguous_basename_suggests_not_resolves_read() {
        let dir = fixture(&["src/a/mod.rs", "src/b/mod.rs"]);
        let res = resolve_fuzzy_path(dir.path(), "mod.rs", GateProfile::Read);
        match res {
            FuzzyResolution::Suggestions(s) => {
                assert!(s.len() >= 2, "expected both mod.rs candidates: {s:?}");
                assert!(s.iter().any(|p| p.contains('a')) && s.iter().any(|p| p.contains('b')));
            }
            other => panic!("expected Suggestions for ambiguous basename, got {other:?}"),
        }
    }

    #[test]
    fn ambiguous_basename_suggests_not_resolves_search() {
        // A path-like query that is an equally-good subsequence of two files.
        let dir = fixture(&["pkg1/a/mod.rs", "pkg2/a/mod.rs"]);
        let res = resolve_fuzzy_path(dir.path(), "a/mod.rs", GateProfile::Search);
        // Two equally-good matches; even path-like, no single-winner margin.
        assert!(
            !matches!(res, FuzzyResolution::Resolved(_)),
            "ambiguous candidates must not auto-open under Search, got {res:?}"
        );
        assert!(matches!(res, FuzzyResolution::Suggestions(_)));
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
            other => panic!("expected Suggestions for 5-way tie, got {other:?}"),
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
    fn search_profile_refuses_non_path_like_query() {
        // `symbol` is a subsequence of src/search/symbol.rs but is NOT path-like
        // (no separator, no extension) — Search must suggest, never auto-open.
        let dir = fixture(&["src/search/symbol.rs"]);
        let res = resolve_fuzzy_path(dir.path(), "symbol", GateProfile::Search);
        assert!(
            !matches!(res, FuzzyResolution::Resolved(_)),
            "non-path-like query must not auto-open under Search"
        );
        assert!(
            matches!(res, FuzzyResolution::Suggestions(_)),
            "non-path-like query with a candidate should suggest"
        );
    }

    #[test]
    fn search_profile_resolves_path_like_single_winner() {
        let dir = fixture(&["src/search/symbol.rs", "src/lib.rs", "README.md"]);
        let res = resolve_fuzzy_path(dir.path(), "serch/symbol.rs", GateProfile::Search);
        assert_eq!(
            resolved_path(&res).as_deref(),
            Some("src/search/symbol.rs"),
            "path-like single-winner query should auto-open under Search"
        );
    }

    // ── Finding 1 security regressions ──────────────────────────────────────

    #[test]
    fn gitignored_file_never_auto_opens() {
        // A file that is listed in .gitignore must not appear as a Resolved
        // result even when the query is a clear near-miss of its basename.
        let dir = tempfile::tempdir().unwrap();
        // Create the real file and the gitignore file
        std::fs::write(dir.path().join(".gitignore"), "secret.env\n").unwrap();
        std::fs::write(dir.path().join("secret.env"), "SECRET=hunter2\n").unwrap();
        // Also put a real (non-ignored) file so there is a candidate pool
        let p = dir.path().join("src");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("main.rs"), "fn main() {}\n").unwrap();
        // Query looks like a near-miss of "secret.env"
        let res = resolve_fuzzy_path(dir.path(), "secre.env", GateProfile::Read);
        assert!(
            !matches!(res, FuzzyResolution::Resolved(ref h) if h.path.to_string_lossy().contains("secret.env")),
            "gitignored file must never be Resolved, even on a near-miss query: {res:?}"
        );
    }

    #[test]
    fn dotfile_never_auto_opens() {
        // A hidden dotfile (e.g. .env) must not auto-open for a near-miss query.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "SECRET=hunter2\n").unwrap();
        let p = dir.path().join("src");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("main.rs"), "fn main() {}\n").unwrap();
        // Queries that are near-misses of ".env"
        for query in &["env", ".en"] {
            let res = resolve_fuzzy_path(dir.path(), query, GateProfile::Read);
            assert!(
                !matches!(res, FuzzyResolution::Resolved(ref h) if h.path.to_string_lossy().contains(".env")),
                "hidden dotfile .env must not be Resolved for query {query:?}: {res:?}"
            );
        }
    }

    // ── Finding 2 regression ────────────────────────────────────────────────

    #[test]
    fn apply_gate_rejects_unique_below_floor() {
        // A single candidate that is the only subsequence match (unique=true)
        // but whose score is below MIN_SCORE must return Suggestions, not
        // Resolved. Before the fix, `clears_floor = unique || score >= floor`
        // let unique candidates bypass the floor entirely.
        let below_floor = READ_MIN_SCORE - 1;
        let scored = vec![(below_floor, PathBuf::from("src/lib.rs"))];
        let res = apply_gate(scored, GateProfile::Read, "lib.rs");
        assert!(
            matches!(res, FuzzyResolution::Suggestions(_)),
            "unique-but-below-floor candidate must return Suggestions, got {res:?}"
        );
    }

    // ── Finding 5 score-threshold regression ────────────────────────────────

    #[test]
    fn canonical_near_miss_scores_above_search_floor() {
        // "serch/symbol.rs" is a deletion-typo of "src/search/symbol.rs".
        // If nucleo's scoring shifts materially this test fails loudly.
        let candidates = vec!["src/search/symbol.rs".to_string()];
        let scored = score_candidates("serch/symbol.rs", &candidates);
        assert!(!scored.is_empty(), "near-miss must be a subsequence match");
        let (score, _) = scored[0];
        assert!(
            score >= SEARCH_MIN_SCORE,
            "near-miss score {score} must be >= SEARCH_MIN_SCORE={SEARCH_MIN_SCORE} \
             (nucleo scoring may have shifted)"
        );
    }

    #[test]
    fn short_incidental_subsequence_scores_below_read_floor() {
        // "ab" matches src/search/symbol.rs only incidentally (score 23 empirically).
        // If nucleo's scoring shifts so that short incidental sequences score
        // above the read floor, we'd be auto-opening unrelated files.
        let candidates = vec!["src/search/symbol.rs".to_string()];
        let scored = score_candidates("ab", &candidates);
        // If it matches at all, assert the score is below the read floor.
        if let Some(&(score, _)) = scored.first() {
            assert!(
                score < READ_MIN_SCORE,
                "short incidental subsequence score {score} must be < READ_MIN_SCORE={READ_MIN_SCORE} \
                 (nucleo scoring may have shifted)"
            );
        }
        // None is also acceptable (no match → safely excluded regardless of floor)
    }

    // ── Finding 7 regressions ────────────────────────────────────────────────

    #[test]
    fn path_like_accepts_slash_without_extension() {
        // config/Dockerfile has a separator but no extension — must be path-like.
        assert!(
            is_path_like("config/Dockerfile"),
            "path with separator but no extension must be path-like"
        );
    }

    #[test]
    fn path_like_accepts_backslash_separator() {
        assert!(
            is_path_like("windows\\path\\file.rs"),
            "path with backslash separator must be path-like"
        );
    }

    #[test]
    fn path_like_rejects_bare_token() {
        assert!(
            !is_path_like("symbol"),
            "bare token without a separator must not be path-like"
        );
        assert!(
            !is_path_like("MyClass"),
            "bare identifier without a separator must not be path-like"
        );
    }

    #[test]
    fn search_profile_resolves_dockerfile_path() {
        // config/Dockerfile is path-like under the new rule (separator present,
        // no extension). A single-winner match should resolve under Search.
        let dir = fixture(&["config/Dockerfile", "src/main.rs"]);
        let res = resolve_fuzzy_path(dir.path(), "config/Dockerfile", GateProfile::Search);
        assert_eq!(
            resolved_path(&res).as_deref(),
            Some("config/Dockerfile"),
            "Dockerfile path (separator, no extension) must resolve under Search"
        );
    }
    impl std::fmt::Debug for FuzzyResolution {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                FuzzyResolution::Resolved(h) => {
                    write!(f, "Resolved({}, score={})", h.path.display(), h.score)
                }
                FuzzyResolution::Suggestions(s) => write!(f, "Suggestions({s:?})"),
                FuzzyResolution::None => write!(f, "None"),
            }
        }
    }
}
