//! Fuzzy, suggest-only symbol names for unresolved plain grok targets.
//!
//! Cold path only — every failed plain-name grok triggers a full-scope walk
//! that reads and parses every `accept_walk_entry`-accepted code file in
//! `scope`, so keep `scope` narrow for a fast miss. Suggest-only: this never
//! resolves a target, it only ranks candidate definition names for a
//! "did you mean" hint. The walk is capped on two independent axes —
//! [`MAX_FUZZY_CANDIDATES`] distinct definition names and [`MAX_FUZZY_FILES`]
//! files read — so a huge or name-sparse scope still terminates; either cap
//! tripping marks the result truncated (a large scope may hide a better match).

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher};

use crate::lang::detect_file_type;
use crate::lang::outline::get_outline_entries;
use crate::search::deps::is_placeholder_name;
use crate::types::{FileType, OutlineEntry, OutlineKind};

/// Upper bound on distinct definition names scored.
const MAX_FUZZY_CANDIDATES: usize = 20_000;
/// Upper bound on the count of files entering the read+parse step. Independent
/// of [`MAX_FUZZY_CANDIDATES`] — a scope of large files with few or repeated
/// definition names would otherwise never trip the name cap and read+parse
/// the entire scope. Counted before the read succeeds or the minified-content
/// gate runs, so it bounds read+parse attempts, not confirmed reads.
const MAX_FUZZY_FILES: usize = 20_000;
const SUGGESTION_K: usize = 3;

/// Warn-once latch: print the truncation notice to stderr at most once per
/// process, so a run of typo'd groks against the same oversized scope doesn't
/// flood stderr with identical lines (same rationale as `timeout.rs`'s
/// abandoned-thread warning, but a plain latch rather than a `==` threshold
/// since there's no count to threshold — just a repeated identical event).
static TRUNCATION_WARNED: AtomicBool = AtomicBool::new(false);

/// Resolve suggestion names for a plain-name grok target that failed to
/// resolve. Returns up to [`SUGGESTION_K`] definition names ranked by fuzzy
/// match against `query`, plus whether the candidate pool was truncated by a
/// cap (so the caller can tell the agent the miss may hide a better match).
/// Returns `None` when nothing in `scope` scores against `query` and the pool
/// was not truncated; when a cap truncated the pool, returns `Some((vec![], true))`
/// even with no scored matches, so the caller can still surface the signal.
pub(crate) fn suggestions(scope: &Path, query: &str) -> Option<(Vec<String>, bool)> {
    let (mut candidates, truncated) = collect_candidates(scope);
    if truncated && !TRUNCATION_WARNED.swap(true, Ordering::Relaxed) {
        eprintln!(
            "tilth: fuzzy symbol resolution hit a cap (max {MAX_FUZZY_CANDIDATES} names / \
             {MAX_FUZZY_FILES} files) for query {query:?} — result may miss a better match"
        );
    }

    // Pre-sort alphabetically so `match_list`'s stable sort breaks equal-score
    // ties alphabetically, making the top-K reproducible run to run.
    candidates.sort_unstable();

    let mut matcher = Matcher::new(Config::DEFAULT);
    let atom = Atom::new(
        query,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
        false,
    );
    let scored = atom.match_list(candidates, &mut matcher);
    if scored.is_empty() {
        return truncated.then_some((Vec::new(), true));
    }
    Some((
        scored
            .into_iter()
            .take(SUGGESTION_K)
            .map(|(name, _)| name)
            .collect(),
        truncated,
    ))
}

/// Walk the `.tilthignore`-pruned tree under `scope` through the same
/// `accept_walk_entry` / secret-file / minified-by-content gates the content
/// and symbol walkers use, collecting distinct definition names from each
/// accepted file's outline. Returns `(candidates, truncated)`; `truncated` is
/// derived after the walk from whichever cap — [`MAX_FUZZY_CANDIDATES`] or
/// [`MAX_FUZZY_FILES`] — was hit (no separate flag needed: the walk state
/// after `Quit` already carries that signal).
fn collect_candidates(scope: &Path) -> (Vec<String>, bool) {
    collect_candidates_capped(scope, MAX_FUZZY_CANDIDATES, MAX_FUZZY_FILES)
}

/// `collect_candidates` with the two caps as parameters — a test seam so unit
/// tests can exercise cap-triggered truncation without walking real
/// [`MAX_FUZZY_CANDIDATES`]/[`MAX_FUZZY_FILES`]-sized fixtures.
fn collect_candidates_capped(
    scope: &Path,
    max_names: usize,
    max_files: usize,
) -> (Vec<String>, bool) {
    let Ok(walker) = crate::search::walker(scope, None) else {
        return (Vec::new(), false);
    };

    let candidates = Mutex::new(HashSet::new());
    let code_files_attempted = AtomicUsize::new(0);

    walker.run(|| {
        let candidates = &candidates;
        let code_files_attempted = &code_files_attempted;
        Box::new(move |entry| {
            let Some((path, file_size)) = crate::search::accept_walk_entry(entry) else {
                return ignore::WalkState::Continue;
            };
            let path = path.as_path();
            if crate::search::path_is_secret_file(path) {
                return ignore::WalkState::Continue;
            }
            let FileType::Code(lang) = detect_file_type(path) else {
                return ignore::WalkState::Continue;
            };
            if code_files_attempted.fetch_add(1, Ordering::Relaxed) >= max_files {
                return ignore::WalkState::Quit;
            }
            let Ok(content) = fs::read_to_string(path) else {
                return ignore::WalkState::Continue;
            };
            if file_size >= crate::lang::detection::MINIFIED_CHECK_THRESHOLD
                && crate::lang::detection::is_minified_by_content(content.as_bytes())
            {
                return ignore::WalkState::Continue;
            }

            let mut names = Vec::new();
            collect_entry_names(&get_outline_entries(&content, lang), &mut names);

            let mut candidates = candidates
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for name in names {
                if candidates.len() >= max_names {
                    return ignore::WalkState::Quit;
                }
                candidates.insert(name);
            }
            ignore::WalkState::Continue
        })
    });

    let candidates = candidates
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let truncated =
        candidates.len() >= max_names || code_files_attempted.load(Ordering::Relaxed) > max_files;
    (candidates.into_iter().collect(), truncated)
}

/// Recursively collect definition names from an outline (excluding imports,
/// exports, and placeholder names — see [`is_placeholder_name`]). No cap
/// applied here; caps are enforced once, at the call site.
fn collect_entry_names(entries: &[OutlineEntry], out: &mut Vec<String>) {
    for entry in entries {
        let is_definition = !matches!(entry.kind, OutlineKind::Import | OutlineKind::Export)
            && !entry.name.is_empty()
            && !is_placeholder_name(&entry.name);
        if is_definition {
            out.push(entry.name.clone());
        }
        collect_entry_names(&entry.children, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_collection_hard_caps_at_max_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("a.rs"),
            "fn alpha() {}\nfn beta() {}\nfn gamma() {}\n",
        )
        .unwrap();

        let (candidates, truncated) = collect_candidates_capped(tmp.path(), 2, 1000);

        assert_eq!(candidates.len(), 2);
        assert!(truncated, "hitting the name cap must report truncated");
    }

    #[test]
    fn candidate_collection_hard_caps_at_max_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.rs"), "fn alpha() {}\n").unwrap();
        fs::write(tmp.path().join("b.rs"), "fn beta() {}\n").unwrap();

        let (_, truncated) = collect_candidates_capped(tmp.path(), 1000, 1);

        assert!(truncated, "hitting the file cap must report truncated");
    }

    #[test]
    fn candidate_collection_reports_untruncated_at_exact_cap_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.rs"), "fn alpha() {}\n").unwrap();
        fs::write(tmp.path().join("b.rs"), "fn beta() {}\n").unwrap();

        let (candidates, truncated) = collect_candidates_capped(tmp.path(), 1000, 2);

        assert_eq!(candidates.len(), 2);
        assert!(
            !truncated,
            "a scope that exactly fills the file cap without overflowing is complete, not truncated"
        );
    }

    #[test]
    fn candidate_collection_filters_non_definition_names() {
        let entries = vec![
            OutlineEntry {
                kind: OutlineKind::Import,
                name: "imported".to_string(),
                start_line: 1,
                end_line: 1,
                signature: None,
                children: vec![OutlineEntry {
                    kind: OutlineKind::Function,
                    name: "nested_real".to_string(),
                    start_line: 1,
                    end_line: 1,
                    signature: None,
                    children: Vec::new(),
                    doc: None,
                }],
                doc: None,
            },
            OutlineEntry {
                kind: OutlineKind::Export,
                name: "exported".to_string(),
                start_line: 1,
                end_line: 1,
                signature: None,
                children: Vec::new(),
                doc: None,
            },
            OutlineEntry {
                kind: OutlineKind::Function,
                name: "<top-level>".to_string(),
                start_line: 1,
                end_line: 1,
                signature: None,
                children: Vec::new(),
                doc: None,
            },
            OutlineEntry {
                kind: OutlineKind::Function,
                name: "impl Foo".to_string(),
                start_line: 1,
                end_line: 1,
                signature: None,
                children: Vec::new(),
                doc: None,
            },
            OutlineEntry {
                kind: OutlineKind::Function,
                name: "real_symbol".to_string(),
                start_line: 1,
                end_line: 1,
                signature: None,
                children: Vec::new(),
                doc: None,
            },
            OutlineEntry {
                kind: OutlineKind::Function,
                name: "x".to_string(),
                start_line: 1,
                end_line: 1,
                signature: None,
                children: Vec::new(),
                doc: None,
            },
        ];
        let mut names = Vec::new();
        collect_entry_names(&entries, &mut names);
        let candidates: HashSet<_> = names.into_iter().collect();
        assert_eq!(
            candidates,
            HashSet::from(["nested_real".to_string(), "real_symbol".to_string()])
        );
    }
}
