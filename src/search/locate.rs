//! Concept → file → symbol locator (the never-refuse "where is this?" engine).
//!
//! Given a bag of intent terms (the concepts an agent has been searching for),
//! rank the repo's FILES by cloud-density, then rank symbols WITHIN the top
//! files. The unit is the FILE: name collision (`matched` is one of hundreds of
//! `match*` symbols) makes symbol-from-intent ranking unreliable, but the
//! answer's *file* is uniquely concept-dense, so file ranking is robust.
//! Validated offline + on real benchmark sessions (see
//! `docs/plans/concept-intent-navigation.md`).
//!
//! Two passes, both language-agnostic:
//!   1. Cheap (no tree-sitter): walk every code file, extract identifiers,
//!      sub-tokenize, score by **file-level IDF coverage + density + path-token
//!      match**. File-IDF zeroes ubiquitous terms (a token in every file can't
//!      discriminate files).
//!   2. On the top-K files only: tree-sitter outline → rank symbols within the
//!      file by name-match + definition weight + exported.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::error::TilthError;
use crate::index::bloom::extract_identifiers;
use crate::lang::detect_file_type;
use crate::lang::outline::get_outline_entries;
use crate::types::{FileType, Lang, OutlineEntry, OutlineKind};

/// A ranked file with its best matching symbols.
#[derive(Debug, Clone)]
pub struct FileRank {
    pub path: PathBuf,
    pub score: i32,
    pub symbols: Vec<SymbolRank>,
}

/// A ranked symbol within a file.
#[derive(Debug, Clone)]
pub struct SymbolRank {
    pub name: String,
    // Retained for parity with the scout's FileRank consumers; only `name` is
    // read since the locate CLI formatters were not ported.
    #[allow(dead_code)]
    pub kind: OutlineKind,
    pub score: i32,
    #[allow(dead_code)]
    pub line: u32,
}

/// Files larger than this are skipped (matches `OutlineCache::get_or_parse`).
const MAX_FILE_BYTES: u64 = 500_000;

/// Split an identifier or term into lowercase sub-tokens, matching the
/// validated tokenizer: `serve_http` → [serve, http]; `ServeHTTP` → [serve,
/// http]; `handleHTTPRequest` → [handle, http, request] (the acronym `HTTP`
/// stays whole, the trailing `Request` splits off). Drops sub-tokens shorter
/// than 2 chars. Pure char state machine — no regex dependency.
pub(crate) fn tokenize(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < len {
        let ch = chars[i];
        if ch.is_ascii_uppercase() {
            // Consume the run of uppercase letters.
            let start = i;
            let mut j = i;
            while j < len && chars[j].is_ascii_uppercase() {
                j += 1;
            }
            if j < len && chars[j].is_ascii_lowercase() {
                // The run is followed by a lowercase word. If the run is >1, the
                // last uppercase belongs to that word (`HTTPRequest` → HTTP +
                // Request); otherwise it's a single CamelWord (`Matcher`).
                let word_start = if j - start > 1 {
                    push_tok(&mut out, &chars[start..j - 1]);
                    j - 1
                } else {
                    start
                };
                let mut end = j;
                while end < len && chars[end].is_ascii_lowercase() {
                    end += 1;
                }
                push_tok(&mut out, &chars[word_start..end]);
                i = end;
            } else {
                // Pure acronym / uppercase run (`HTTP`, `IO`).
                push_tok(&mut out, &chars[start..j]);
                i = j;
            }
        } else if ch.is_ascii_lowercase() {
            let start = i;
            let mut j = i;
            while j < len && chars[j].is_ascii_lowercase() {
                j += 1;
            }
            push_tok(&mut out, &chars[start..j]);
            i = j;
        } else if ch.is_ascii_digit() {
            let start = i;
            let mut j = i;
            while j < len && chars[j].is_ascii_digit() {
                j += 1;
            }
            push_tok(&mut out, &chars[start..j]);
            i = j;
        } else {
            // Separator (`_`, `.`, whitespace, punctuation).
            i += 1;
        }
    }
    out
}

fn push_tok(out: &mut Vec<String>, chars: &[char]) {
    if chars.len() >= 2 {
        out.push(chars.iter().collect::<String>().to_lowercase());
    }
}

/// Per-file data gathered in pass 1 (no tree-sitter).
struct FileScan {
    path: PathBuf,
    lang: Lang,
    /// Intent tokens present in the file (subset of the intent set).
    present: Vec<String>,
    /// Count of identifier sub-token occurrences that are intent tokens.
    hits: usize,
    /// Total identifier sub-token occurrences (for density normalization).
    total: usize,
}

/// Rank the files under `scope` by how well they match `intent`, returning the
/// top `top_k` with their best symbols. Never errors on individual unreadable
/// files (they are skipped); only a failed directory walk propagates.
pub fn locate(intent: &[&str], scope: &Path, top_k: usize) -> Result<Vec<FileRank>, TilthError> {
    // Build the intent token set (sub-tokenized, deduped).
    let intent_tokens: HashSet<String> = intent.iter().flat_map(|t| tokenize(t)).collect();
    if intent_tokens.is_empty() || top_k == 0 {
        return Ok(Vec::new());
    }

    // --- Pass 1: scan every code file in parallel (no tree-sitter). ---
    let intent_arc = Arc::new(intent_tokens.clone());
    let scans: Arc<Mutex<Vec<FileScan>>> = Arc::new(Mutex::new(Vec::new()));
    let walker = super::walker(scope, None)?;
    walker.run(|| {
        let intent = Arc::clone(&intent_arc);
        let scans = Arc::clone(&scans);
        Box::new(move |entry| {
            if let Ok(entry) = entry {
                if entry.file_type().is_some_and(|ft| ft.is_file()) {
                    if let Some(scan) = scan_file(entry.path(), &intent) {
                        scans.lock().unwrap().push(scan);
                    }
                }
            }
            ignore::WalkState::Continue
        })
    });
    let scans = Arc::try_unwrap(scans)
        .map(|m| m.into_inner().unwrap())
        .unwrap_or_default();
    if scans.is_empty() {
        return Ok(Vec::new());
    }

    // File-level IDF: a token in many files can't discriminate files.
    let n_files = scans.len() as f64;
    let mut df: HashMap<&str, usize> = HashMap::new();
    for s in &scans {
        for t in &s.present {
            *df.entry(t.as_str()).or_insert(0) += 1;
        }
    }
    let idf = |t: &str| -> f64 { ((n_files + 1.0) / (*df.get(t).unwrap_or(&0) as f64 + 1.0)).ln() };

    // Score each file: IDF coverage + density + path-token match.
    let mut scored: Vec<(f64, &FileScan)> = scans
        .iter()
        .map(|s| {
            let coverage: f64 = s.present.iter().map(|t| idf(t)).sum();
            let density = s.hits as f64 / (s.total.max(1) as f64).sqrt();
            let path_score: f64 = path_tokens(&s.path, scope)
                .iter()
                .filter(|t| intent_tokens.contains(*t))
                .map(|t| idf(t))
                .sum();
            let mut score = coverage + 0.5 * density + 3.0 * path_score;
            // Test files are concept-dense but rarely the navigation target —
            // demote (don't exclude: a test CAN be the answer for a test task).
            if is_test_path(&s.path) {
                score *= 0.3;
            }
            (score, s)
        })
        .filter(|(score, _)| *score > 0.0)
        .collect();
    // Deterministic order: score desc, then path asc.
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.path.cmp(&b.1.path))
    });

    // --- Pass 2: tree-sitter outline on the top-K files only. ---
    let out: Vec<FileRank> = scored
        .into_iter()
        .take(top_k)
        .map(|(score, s)| FileRank {
            path: s.path.clone(),
            score: (score * 100.0).round() as i32,
            symbols: rank_symbols(&s.path, s.lang, &intent_tokens, &idf),
        })
        .collect();
    Ok(out)
}

/// Pass-1 work for one file: read, extract identifiers, sub-tokenize, count
/// intent hits. Returns `None` for non-code files, oversized files, or read
/// failures.
fn scan_file(path: &Path, intent: &HashSet<String>) -> Option<FileScan> {
    let FileType::Code(lang) = detect_file_type(path) else {
        return None;
    };
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    let mut present: HashSet<String> = HashSet::new();
    let mut hits = 0usize;
    let mut total = 0usize;
    for ident in extract_identifiers(&content, Some(lang)) {
        for tok in tokenize(ident) {
            total += 1;
            if intent.contains(&tok) {
                hits += 1;
                present.insert(tok);
            }
        }
    }
    // Keep the file if it matches in content OR in its path (a file can be the
    // answer purely by living in a concept-named dir, e.g. `matcher/lib.rs`).
    let path_hit = path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| tokenize(s).iter().any(|t| intent.contains(t)))
    });
    if present.is_empty() && !path_hit {
        return None;
    }
    Some(FileScan {
        path: path.to_path_buf(),
        lang,
        present: present.into_iter().collect(),
        hits,
        total,
    })
}

/// Whether a path looks like a test file. Extends tilth's `is_test_file`
/// (`.test.`/`.spec.`/`__tests__/`) with the `tests/` dir and `test_*`/`*_test`
/// filename conventions that real repos use (e.g. comrak's `src/tests/`).
pub(crate) fn is_test_path(path: &Path) -> bool {
    if crate::types::is_test_file(path) {
        return true;
    }
    if path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .any(|c| matches!(c, "tests" | "test" | "testdata" | "__tests__"))
    {
        return true;
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|stem| {
            stem.starts_with("test_")
                || stem.ends_with("_test")
                || stem.ends_with("_tests")
                || stem == "tests"
        })
}

/// Whether a path is nav-noise: a test, example, benchmark, or fuzz file. These
/// are almost never the navigation anchor for a comprehension/trace prompt, yet
/// their prose-y content pollutes ranking — so the scout drops them from its
/// pool and from skeleton callers/callees.
pub(crate) fn is_nav_noise(path: &Path) -> bool {
    is_test_path(path)
        || path.components().any(|c| {
            c.as_os_str().to_str().is_some_and(|s| {
                matches!(
                    s.trim_start_matches(['_', '.']),
                    "examples" | "example" | "benches" | "bench" | "fuzz"
                )
            })
        })
}

/// Sub-tokens of a file's path relative to `scope` (directory and file-stem
/// names) — `crates/regex/src/matcher.rs` → {crates, regex, src, matcher}.
fn path_tokens(path: &Path, scope: &Path) -> HashSet<String> {
    let rel = path.strip_prefix(scope).unwrap_or(path);
    let mut toks = HashSet::new();
    for comp in rel.components() {
        if let Some(s) = comp.as_os_str().to_str() {
            // Drop the extension by tokenizing the stem portion too; tokenize
            // already splits on `.`, so passing the whole component is fine.
            for t in tokenize(s) {
                toks.insert(t);
            }
        }
    }
    toks
}

/// Pass-2 work: outline the file and rank its symbols by name-match + kind +
/// exported. Returns the top 3.
fn rank_symbols(
    path: &Path,
    lang: Lang,
    intent: &HashSet<String>,
    idf: &impl Fn(&str) -> f64,
) -> Vec<SymbolRank> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let entries = get_outline_entries(&content, lang);
    let mut flat = Vec::new();
    flatten(&entries, &mut flat);

    let mut ranked: Vec<SymbolRank> = flat
        .iter()
        .filter_map(|e| {
            let name_toks = tokenize(&e.name);
            // Per-hit weight is `1 + idf`: within a file we're past file
            // discrimination, so a name match must always count even when the
            // token's file-IDF is ~0 (e.g. a small repo where it's everywhere).
            let hits: Vec<&String> = name_toks.iter().filter(|t| intent.contains(*t)).collect();
            if hits.is_empty() {
                return None;
            }
            let name_score: f64 = hits.iter().map(|t| 1.0 + idf(t)).sum();
            let exported = e.signature.as_deref().is_some_and(is_exported_signature);
            let score = 5.0 * name_score
                + f64::from(kind_weight(e.kind))
                + if exported { 30.0 } else { 0.0 };
            Some(SymbolRank {
                name: e.name.clone(),
                kind: e.kind,
                score: (score * 100.0).round() as i32,
                line: e.start_line,
            })
        })
        .collect();
    ranked.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.name.cmp(&b.name)));
    ranked.truncate(3);
    ranked
}

/// Flatten the nested outline (top-level entries + their children) into a flat
/// list, skipping imports (never a navigation target).
fn flatten<'a>(entries: &'a [OutlineEntry], out: &mut Vec<&'a OutlineEntry>) {
    for e in entries {
        if !matches!(e.kind, OutlineKind::Import) {
            out.push(e);
        }
        flatten(&e.children, out);
    }
}

/// Semantic weight per outline kind — mirrors `treesitter::definition_weight`
/// but over the post-processed `OutlineKind` enum. Definitions rank highest;
/// imports/exports/variables low (rarely the navigation target).
fn kind_weight(kind: OutlineKind) -> u16 {
    match kind {
        OutlineKind::Function
        | OutlineKind::Class
        | OutlineKind::Struct
        | OutlineKind::Interface
        | OutlineKind::Enum
        | OutlineKind::TypeAlias => 100,
        OutlineKind::Constant => 80,
        OutlineKind::Module | OutlineKind::Property => 70,
        OutlineKind::TestSuite | OutlineKind::TestCase => 50,
        OutlineKind::Variable | OutlineKind::ImmutableVariable => 40,
        OutlineKind::Export => 30,
        OutlineKind::Import => 10,
    }
}

/// Whether a signature line marks an exported/public symbol.
fn is_exported_signature(sig: &str) -> bool {
    let s = sig.trim_start();
    s.starts_with("pub ")
        || s.starts_with("pub(")
        || s.starts_with("export ")
        || s.starts_with("export default ")
        || s.starts_with("public ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn tmpdir(tag: &str) -> PathBuf {
        static C: AtomicU64 = AtomicU64::new(0);
        let n = C.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!(
            "tilth_locate_test_{tag}_{}_{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, content).unwrap();
    }
    fn names(fr: &FileRank) -> Vec<&str> {
        fr.path
            .file_name()
            .and_then(|s| s.to_str())
            .into_iter()
            .collect()
    }

    #[test]
    fn tokenize_splits_camel_snake_and_acronyms() {
        assert_eq!(tokenize("serve_http"), vec!["serve", "http"]);
        assert_eq!(tokenize("ServeHTTP"), vec!["serve", "http"]);
        assert_eq!(
            tokenize("handleHTTPRequest"),
            vec!["handle", "http", "request"]
        );
        assert_eq!(tokenize("RegexMatcher"), vec!["regex", "matcher"]);
        assert_eq!(tokenize("parse_document"), vec!["parse", "document"]);
        assert_eq!(tokenize("find_at"), vec!["find", "at"]);
        // 1-char fragments dropped.
        assert_eq!(tokenize("a_b_cd"), vec!["cd"]);
    }

    #[test]
    fn ranks_concept_dense_file_above_unrelated() {
        let d = tmpdir("dense");
        write(&d, "printer.rs", "fn matched(sink: Sink, line: Line) { write_output(sink, line) }\nfn flush(sink: Sink) {}");
        write(
            &d,
            "config.rs",
            "fn load_config() {}\nstruct Settings { verbose: bool }",
        );
        let out = locate(&["sink", "line", "output"], &d, 5).unwrap();
        assert!(!out.is_empty(), "expected ranked files");
        assert_eq!(
            names(&out[0]),
            vec!["printer.rs"],
            "concept-dense file must rank first; got {:?}",
            out.iter().map(|f| f.path.clone()).collect::<Vec<_>>()
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn ubiquitous_token_does_not_decide_ranking() {
        // `ctx` appears in EVERY file → file-IDF ~0 → it can't pick a winner.
        // The rare token `decode` must decide.
        let d = tmpdir("idf");
        write(&d, "a.rs", "fn alpha(ctx: Ctx) { ctx.run() }");
        write(&d, "b.rs", "fn beta(ctx: Ctx) { ctx.run() }");
        write(
            &d,
            "mapper.rs",
            "fn decode(ctx: Ctx, raw: Raw) { ctx.set(raw) }",
        );
        let out = locate(&["ctx", "decode"], &d, 5).unwrap();
        assert_eq!(
            names(&out[0]),
            vec!["mapper.rs"],
            "rare token must win over ubiquitous; got {:?}",
            out.iter().map(|f| f.path.clone()).collect::<Vec<_>>()
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn path_token_match_boosts_file() {
        let d = tmpdir("path");
        write(&d, "matcher/lib.rs", "fn run() { let x = 1; }");
        write(&d, "other/lib.rs", "fn run() { let x = 1; }");
        // Identical bodies; only the PATH distinguishes them.
        let out = locate(&["matcher"], &d, 5).unwrap();
        assert!(
            out[0].path.to_string_lossy().contains("matcher/"),
            "path-token match must win; got {:?}",
            out[0].path
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn ranks_symbols_within_file_by_name_match() {
        let d = tmpdir("syms");
        write(&d, "render.rs", "fn helper() {}\nfn format_node_default(node: Node) { write_html(node) }\nfn unrelated() {}");
        let out = locate(&["format", "node", "html"], &d, 1).unwrap();
        assert_eq!(
            out[0].symbols.first().map(|s| s.name.as_str()),
            Some("format_node_default"),
            "best symbol must be the name-matching one; got {:?}",
            out[0].symbols
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn works_across_languages_python_and_go() {
        // Proves language-agnostic: a Python file and a Go file, intent matches Python.
        let d = tmpdir("lang");
        write(
            &d,
            "resolver.py",
            "def resolve_dependency(scope):\n    return inject(scope)\n",
        );
        write(&d, "server.go", "func ServeHTTP(w Writer, r Request) {}\n");
        let out = locate(&["resolve", "dependency", "inject"], &d, 5).unwrap();
        assert_eq!(
            names(&out[0]),
            vec!["resolver.py"],
            "Python file must rank for Python-matching intent; got {:?}",
            out.iter().map(|f| f.path.clone()).collect::<Vec<_>>()
        );
        assert!(
            out[0]
                .symbols
                .iter()
                .any(|s| s.name == "resolve_dependency"),
            "Python symbol must be extracted; got {:?}",
            out[0].symbols
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn test_files_are_demoted_below_equally_dense_source() {
        let d = tmpdir("testpen");
        // Same concept density; the source file must outrank the test file.
        write(
            &d,
            "src/inlines.rs",
            "fn parse_inline(node: Node) { inline(node) }",
        );
        write(
            &d,
            "src/tests/inline_cases.rs",
            "fn test_parse_inline() { let node = inline(); }",
        );
        let out = locate(&["parse", "inline", "node"], &d, 5).unwrap();
        assert!(
            out[0].path.to_string_lossy().contains("src/inlines.rs"),
            "source must outrank test file; got {:?}",
            out.iter().map(|f| f.path.clone()).collect::<Vec<_>>()
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn empty_intent_and_no_match_return_empty() {
        let d = tmpdir("empty");
        write(&d, "a.rs", "fn alpha() {}");
        assert!(
            locate(&[], &d, 5).unwrap().is_empty(),
            "empty intent → empty"
        );
        assert!(
            locate(&["nonexistent_zzz"], &d, 5).unwrap().is_empty(),
            "no match → empty"
        );
        std::fs::remove_dir_all(&d).ok();
    }
}
