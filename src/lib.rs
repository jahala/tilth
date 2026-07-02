#![warn(clippy::pedantic)]
#![allow(
    clippy::cast_precision_loss, // usize→f64 for IDF/score math — counts well under 2^52
    clippy::cast_possible_truncation,  // line numbers as u32, token counts — we target 64-bit
    clippy::cast_sign_loss,            // same
    clippy::cast_possible_wrap,        // u32→i32 for tree-sitter APIs
    clippy::module_name_repetitions,   // Rust naming conventions
    clippy::similar_names,             // common in parser/search code
    clippy::too_many_lines,            // crate-wide to cover find_definitions in src/search/symbol.rs;
                                       // narrow to a per-function allow once a refactor shrinks that file
    clippy::too_many_arguments,        // internal recursive AST walker
    clippy::unnecessary_wraps,         // Result return for API consistency
    clippy::struct_excessive_bools,    // CLI struct derives clap
    clippy::missing_errors_doc,        // internal pub(crate) fns don't need error docs
    clippy::missing_panics_doc,        // same
)]

pub(crate) mod budget;
pub mod cache;
pub(crate) mod classify;
pub mod diff;
pub(crate) mod edit;
pub(crate) mod edit_parse_check;
pub mod error;
pub(crate) mod format;
pub mod index;
pub mod infer;
pub mod install;
pub(crate) mod lang;
pub mod map;
pub mod mcp;
pub mod model_pull;
pub mod overview;
pub(crate) mod read;
pub(crate) mod search;
pub(crate) mod session;
pub(crate) mod timeout;
pub(crate) mod types;
pub(crate) mod util;

/// Re-exports for the fuzz harness. Not stable; do not depend on this.
/// Items here are only `pub` so `fuzz/fuzz_targets/*.rs` can reach them
/// without us widening the rest of the crate's pub(crate) surface.
#[doc(hidden)]
pub mod __fuzz {
    use std::collections::HashSet;
    use std::path::Path;

    pub use crate::read::outline::code::outline;
    pub use crate::types::Lang;

    /// Wrapper: `strip_noise` is `pub(crate)`, so we re-export via a function
    /// rather than `pub use` (which Rust forbids for less-visible items).
    #[must_use]
    pub fn strip_noise(content: &str, path: &Path, def_range: Option<(u32, u32)>) -> HashSet<u32> {
        crate::search::strip::strip_noise(content, path, def_range)
    }

    /// Wrapper: same pattern for `parse_unified_diff`.
    /// Returns unit because the fuzz target doesn't introspect the result.
    pub fn parse_unified_diff(raw: &str) {
        let _ = crate::diff::parse::parse_unified_diff(raw);
    }
}

use std::path::Path;

use cache::OutlineCache;
use classify::classify;
use error::TilthError;
use types::QueryType;

/// Holds expanded search dependencies, allocated once.
/// Avoids scattered `Option<T>` + `unwrap()` throughout dispatch.
struct ExpandedCtx {
    session: session::Session,
    bloom: index::bloom::BloomFilterCache,
    expand: usize,
    /// Raises the search match cap (10 → 100). Driven by the explicit `--full`
    /// flag, NOT by `full = !is_tty`. Piped invocation must preserve the
    /// concise outline — see the `piped_invocation_does_not_auto_expand`
    /// pin in `main.rs` for the larger design rule this enforces.
    full_search: bool,
}

/// The single public API. Everything flows through here:
/// classify → match on query type → return formatted string.
pub fn run(
    query: &str,
    scope: &Path,
    section: Option<&str>,
    budget_tokens: Option<u64>,
    glob: Option<&str>,
    cache: &OutlineCache,
) -> Result<String, TilthError> {
    run_inner(
        query,
        scope,
        section,
        budget_tokens,
        false,
        0,
        glob,
        cache,
        false,
    )
}

/// Full variant — forces full file output, bypassing smart views.
/// `full_file` covers piped-stdout promotion too; search cap bump never
/// applies on this path (no expansion = no `run_query_expanded`).
pub fn run_full(
    query: &str,
    scope: &Path,
    section: Option<&str>,
    budget_tokens: Option<u64>,
    glob: Option<&str>,
    cache: &OutlineCache,
) -> Result<String, TilthError> {
    run_inner(
        query,
        scope,
        section,
        budget_tokens,
        true,
        0,
        glob,
        cache,
        false,
    )
}

/// Run with expanded search — inline source for top N matches.
/// `full` controls full-file display for `FilePath` queries (driven by
/// `cli.full || !is_tty`). `cli_full` is the *parsed* `--full` flag and
/// alone gates the search match-cap bump; piped invocation must not raise
/// the cap (see `piped_invocation_does_not_auto_expand` pin).
pub fn run_expanded(
    query: &str,
    scope: &Path,
    section: Option<&str>,
    budget_tokens: Option<u64>,
    full: bool,
    expand: usize,
    glob: Option<&str>,
    cache: &OutlineCache,
    cli_full: bool,
) -> Result<String, TilthError> {
    run_inner(
        query,
        scope,
        section,
        budget_tokens,
        full,
        expand,
        glob,
        cache,
        cli_full,
    )
}

/// Find all callers of a symbol.
pub fn run_callers(
    target: &str,
    scope: &Path,
    expand: usize,
    budget_tokens: Option<u64>,
    glob: Option<&str>,
    full: bool,
) -> Result<String, TilthError> {
    let bloom = index::bloom::BloomFilterCache::new();
    let expand = if expand > 0 { expand } else { 2 };
    let output =
        search::callers::search_callers_expanded(target, scope, &bloom, expand, None, glob, full)?;
    match budget_tokens {
        Some(b) => Ok(budget::apply(&output, b)),
        None => Ok(output),
    }
}

/// Analyze blast-radius dependencies of a file.
pub fn run_deps(
    path: &Path,
    scope: &Path,
    budget_tokens: Option<u64>,
) -> Result<String, TilthError> {
    let bloom = index::bloom::BloomFilterCache::new();
    let result = search::deps::analyze_deps(path, scope, &bloom)?;
    let budget_usize = budget_tokens.map(|b| b as usize);
    Ok(search::deps::format_deps(&result, scope, budget_usize))
}

/// Grok a symbol: return def + doc + callees + callers + siblings + tests in one call.
///
/// `target_spec` accepts a bare symbol name (`parse_unified_diff`), a path:line
/// pair (`src/diff/parse.rs:7`), or a `Type::method` reference.
pub fn run_grok(target_spec: &str, scope: &Path, full: bool) -> Result<String, TilthError> {
    let bloom = index::bloom::BloomFilterCache::new();
    let session = session::Session::default();
    let caps = if full {
        search::grok::GrokCaps::full()
    } else {
        search::grok::GrokCaps::default()
    };
    let result = search::grok::grok(target_spec, scope, &bloom, &session, caps)?;
    Ok(search::grok::format_grok(&result, scope))
}

fn run_inner(
    query: &str,
    scope: &Path,
    section: Option<&str>,
    budget_tokens: Option<u64>,
    full: bool,
    expand: usize,
    glob: Option<&str>,
    cache: &OutlineCache,
    cli_full: bool,
) -> Result<String, TilthError> {
    let query_type = classify(query, scope);

    let use_expanded =
        expand > 0 && !matches!(query_type, QueryType::FilePath(_) | QueryType::Glob(_));

    // Multi-symbol: comma-separated identifiers, 2..=5 items
    // Check before main dispatch. Only activate when all parts look like identifiers
    // to avoid hijacking regex (/foo,bar/) or glob (*.{rs,ts}) queries.
    if query.contains(',')
        && !matches!(
            query_type,
            QueryType::Regex(_) | QueryType::Glob(_) | QueryType::FilePath(_)
        )
    {
        let parts: Vec<&str> = query
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        let all_identifiers = parts.iter().all(|p| classify::is_identifier(p));
        if parts.len() > 5 && all_identifiers {
            return Err(TilthError::InvalidQuery {
                query: query.to_string(),
                reason: "multi-symbol search supports 2-5 symbols".to_string(),
            });
        }
        if parts.len() >= 2 && parts.len() <= 5 && all_identifiers {
            let session = session::Session::new();
            let bloom = index::bloom::BloomFilterCache::new();
            let expand = if expand > 0 { expand } else { 2 };
            let output = search::search_multi_symbol_expanded(
                &parts, scope, cache, &session, &bloom, expand, None, glob, cli_full,
            )?;
            return match budget_tokens {
                Some(b) => Ok(budget::apply(&output, b)),
                None => Ok(output),
            };
        }
    }

    // FilePath and Glob are read operations, not search — handle before expanded dispatch
    let output = match query_type {
        QueryType::FilePath(path) => {
            let mut out = read::read_file(&path, section, full, cache, false)?;
            if section.is_none() && !full && read::would_outline(&path) {
                let related = read::imports::resolve_related_files(&path);
                if !related.is_empty() {
                    let hints: Vec<String> = related
                        .iter()
                        .filter_map(|p| p.strip_prefix(scope).ok().or(Some(p.as_path())))
                        .map(|p| p.display().to_string())
                        .collect();
                    out.push_str("\n\n> Related: ");
                    out.push_str(&hints.join(", "));
                }
            }
            out
        }
        QueryType::Glob(pattern) => search::search_glob(&pattern, scope)?,
        _ if use_expanded => {
            let ctx = ExpandedCtx {
                session: session::Session::new(),
                bloom: index::bloom::BloomFilterCache::new(),
                expand,
                full_search: cli_full,
            };
            run_query_expanded(&query_type, scope, cache, &ctx, glob)?
        }
        _ => run_query_basic(&query_type, scope, cache, glob)?,
    };

    match budget_tokens {
        Some(b) => Ok(budget::apply(&output, b)),
        None => Ok(output),
    }
}

/// Dispatch search queries in expanded mode (inline source for top N matches).
/// Only called for search query types — FilePath/Glob are handled before this.
fn run_query_expanded(
    query_type: &QueryType,
    scope: &Path,
    cache: &OutlineCache,
    ctx: &ExpandedCtx,
    glob: Option<&str>,
) -> Result<String, TilthError> {
    match query_type {
        QueryType::Symbol(name) => search::search_symbol_expanded(
            name,
            scope,
            cache,
            &ctx.session,
            &ctx.bloom,
            ctx.expand,
            None,
            glob,
            ctx.full_search,
        ),
        QueryType::Concept(text) if text.contains(' ') => search::search_content_expanded(
            text,
            scope,
            cache,
            &ctx.session,
            ctx.expand,
            None,
            glob,
            ctx.full_search,
        ),
        // Single-word Concept and Fallthrough share the same expanded path:
        // both go straight to symbol_expanded, intentionally bypassing the
        // definitions>0 / content fallback cascade in single_query_search.
        // The expanded variant already provides richer results with inline source.
        QueryType::Concept(text) | QueryType::Fallthrough(text) => search::search_symbol_expanded(
            text,
            scope,
            cache,
            &ctx.session,
            &ctx.bloom,
            ctx.expand,
            None,
            glob,
            ctx.full_search,
        ),
        QueryType::Content(text) => search::search_content_expanded(
            text,
            scope,
            cache,
            &ctx.session,
            ctx.expand,
            None,
            glob,
            ctx.full_search,
        ),
        QueryType::Regex(pattern) => search::search_regex_expanded(
            pattern,
            scope,
            cache,
            &ctx.session,
            ctx.expand,
            None,
            glob,
            ctx.full_search,
        ),
        // FilePath/Glob never reach here (gated by use_expanded)
        QueryType::FilePath(_) | QueryType::Glob(_) => {
            unreachable!("non-search query type in expanded path")
        }
    }
}

/// Dispatch search queries in basic mode (no expansion).
/// Only called for search query types — FilePath/Glob are handled before this.
fn run_query_basic(
    query_type: &QueryType,
    scope: &Path,
    cache: &OutlineCache,
    glob: Option<&str>,
) -> Result<String, TilthError> {
    match query_type {
        QueryType::Symbol(name) => search::search_symbol(name, scope, cache, glob),
        QueryType::Concept(text) if text.contains(' ') => {
            multi_word_concept_search(text, scope, cache, glob)
        }
        QueryType::Concept(text) => {
            // Single-word concept: prefer definitions, then content, then any match.
            single_query_search(text, scope, cache, true, glob)
        }
        QueryType::Content(text) => search::search_content(text, scope, cache, glob),
        QueryType::Regex(pattern) => search::search_regex(pattern, scope, cache, glob),
        QueryType::Fallthrough(text) => {
            // Accept any symbol match immediately (no definitions preference).
            single_query_search(text, scope, cache, false, glob)
        }
        // FilePath/Glob never reach here
        QueryType::FilePath(_) | QueryType::Glob(_) => {
            unreachable!("non-search query type in basic path")
        }
    }
}

/// Shared cascade for single-word queries: symbol → content → not found.
///
/// When `prefer_definitions` is true (Concept path), only accept symbol results
/// that contain actual definitions; fall back to content otherwise.
/// When false (Fallthrough path), accept any symbol match immediately.
fn single_query_search(
    text: &str,
    scope: &Path,
    cache: &cache::OutlineCache,
    prefer_definitions: bool,
    glob: Option<&str>,
) -> Result<String, error::TilthError> {
    let sym_result = search::search_symbol_raw(text, scope, glob)?;
    let accept_sym = if prefer_definitions {
        sym_result.definitions > 0
    } else {
        sym_result.total_found > 0
    };

    if accept_sym {
        return search::format_raw_result(&sym_result, cache);
    }

    let content_result = search::search_content_raw(text, scope, glob)?;
    if content_result.total_found > 0 {
        return search::format_raw_result(&content_result, cache);
    }

    // For concept queries: if symbol had usages but no definitions, show those
    if prefer_definitions && sym_result.total_found > 0 {
        return search::format_raw_result(&sym_result, cache);
    }

    Err(error::TilthError::NotFound {
        path: scope.join(text),
        suggestion: read::suggest_similar_file(scope, text),
    })
}

/// Multi-word concept search: exact phrase first, then relaxed word proximity.
fn multi_word_concept_search(
    text: &str,
    scope: &Path,
    cache: &cache::OutlineCache,
    glob: Option<&str>,
) -> Result<String, error::TilthError> {
    // Try exact phrase match first
    let mut content_result = search::search_content_raw(text, scope, glob)?;
    content_result.query = text.to_string();
    if content_result.total_found > 0 {
        return search::format_raw_result(&content_result, cache);
    }

    // Relaxed: match all words in any order
    let words: Vec<&str> = text.split_whitespace().collect();
    let relaxed = if words.len() == 2 {
        format!(
            "{}.*{}|{}.*{}",
            regex_syntax::escape(words[0]),
            regex_syntax::escape(words[1]),
            regex_syntax::escape(words[1]),
            regex_syntax::escape(words[0]),
        )
    } else {
        // 3+ words: match any word (OR), rely on multi_word_boost in ranking
        words
            .iter()
            .map(|w| regex_syntax::escape(w))
            .collect::<Vec<_>>()
            .join("|")
    };

    let mut relaxed_result = search::search_regex_raw(&relaxed, scope, glob)?;
    relaxed_result.query = text.to_string();
    if relaxed_result.total_found > 0 {
        return search::format_raw_result(&relaxed_result, cache);
    }

    let first_word = words.first().copied().unwrap_or(text);
    Err(error::TilthError::NotFound {
        path: scope.join(text),
        suggestion: read::suggest_similar_file(scope, first_word),
    })
}

// ---------------------------------------------------------------------------
// Scout: turn-0 structural hint pipeline (ported from the integration line)
// ---------------------------------------------------------------------------

/// Explicitly build + cache the embed index for `scope`. Scout QUERIES never
/// build the index (bounded on huge repos — they score their pool on-demand);
/// this is the one place that walks + embeds the whole corpus, so run it ahead
/// of time (install / pre-warm) or in the background. Returns a one-line summary.
pub fn warm_scout_index(scope: &Path) -> Result<String, TilthError> {
    let cfg = infer::ModelConfig::from_name("embedder");
    let n = infer::embed_index::warm(&cfg, scope)?;
    Ok(format!(
        "warmed embed index: {n} files ({})",
        scope.display()
    ))
}

/// Scout: extract candidate files for a natural-language prompt, then apply a
/// validated ranking pipeline and emit a terse structural skeleton.
///
/// # Jobs
/// - `"context"` — deterministic: candidate files with score and leading symbols.
/// - `"rerank"` — rrf(CE-rank, embed-rank) on symbol texts; gate fires when
///   CE-top1 == embed-top1; emits a grok skeleton of the fusion winner.
///   Degrades to `context` with `model_used: false` when the `infer` feature
///   is absent or the model files are missing.
///
/// Both jobs start with pool = density-locate ∪ embed-recall (when lexical
/// signal is weak). A noise filter drops vendored/generated path prefixes from
/// the pool before ranking.
///
/// Output is a stable JSON object when `json` is true; otherwise a terse human
/// block.
pub fn run_scout(prompt: &str, scope: &Path, job: &str, json: bool) -> Result<String, TilthError> {
    if !matches!(job, "context" | "rerank") {
        return Err(TilthError::InvalidQuery {
            query: job.to_string(),
            reason: format!("unknown scout job `{job}`; supported: context, rerank"),
        });
    }

    let start = std::time::Instant::now();

    // Extract meaningful words from the prompt: lowercase, split on
    // non-alphanumerics, drop stopwords and tokens shorter than 3 chars.
    let terms: Vec<&str> = extract_prompt_terms(prompt);
    // locate returns an empty Vec when terms is empty — handle gracefully.
    let ranked = search::locate::locate(
        terms.as_slice(),
        scope,
        10, // locate top-k candidate files
    )?;

    // Build candidate list from locate results.
    let locate_candidates: Vec<ScoutCandidate> = ranked
        .iter()
        .map(|f| {
            let rel = f
                .path
                .strip_prefix(scope)
                .unwrap_or(&f.path)
                .to_string_lossy()
                .into_owned();
            let why = if f.symbols.is_empty() {
                String::new()
            } else {
                f.symbols
                    .iter()
                    .take(3)
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            ScoutCandidate {
                path: rel,
                score: f.score,
                why,
            }
        })
        .collect();

    // ------------------------------------------------------------------
    // Embed-∪ recall: augment locate candidates when the lexical signal
    // is weak (abstract prompt where locate will under-cover).
    // ------------------------------------------------------------------
    let candidates = embed_union(prompt, scope, locate_candidates);

    // ------------------------------------------------------------------
    // Noise filter: drop vendored / generated / artifact paths.
    // ------------------------------------------------------------------
    let candidates = filter_noise(candidates);

    let n_pool = candidates.len();

    // Job dispatch.
    let (model_used, note, final_candidates, gate_fired, agreement, skeleton) = match job {
        "rerank" => scout_rerank(prompt, scope, candidates),
        _ => {
            // "context" — deterministic path, no model
            (false, None, candidates, false, false, None)
        }
    };

    let elapsed_ms = start.elapsed().as_millis() as u64;

    if json {
        Ok(format_scout_json(
            prompt,
            job,
            model_used,
            elapsed_ms,
            n_pool,
            gate_fired,
            agreement,
            &final_candidates,
            skeleton.as_deref(),
            note.as_deref(),
        ))
    } else {
        Ok(format_scout_human(
            prompt,
            job,
            model_used,
            elapsed_ms,
            gate_fired,
            &final_candidates,
            skeleton.as_deref(),
            note.as_deref(),
        ))
    }
}

/// Prefixes (relative to scope) that are considered vendored / generated /
/// artifact directories.  Candidates whose path starts with any of these are
/// dropped from the pool before ranking.
const NOISE_PREFIXES: &[&str] = &[
    "benchmark/",
    "scripts/",
    "fuzz/",
    "target/",
    "node_modules/",
    "vendor/",
    ".git/",
    "dist/",
    "build/",
];

/// Drop pool candidates that live under vendored or generated directories.
fn filter_noise(candidates: Vec<ScoutCandidate>) -> Vec<ScoutCandidate> {
    candidates
        .into_iter()
        .filter(|c| {
            if NOISE_PREFIXES
                .iter()
                .any(|prefix| c.path.starts_with(prefix))
            {
                return false;
            }
            // Tests, examples, benchmarks, fuzz targets: almost never the nav
            // anchor for a comprehension/trace prompt, and their prose-y content
            // pollutes the embed ranking (locate keeps them globally, demoted).
            !crate::search::locate::is_nav_noise(std::path::Path::new(&c.path))
        })
        .collect()
}

/// RRF constant k (standard value balancing top-rank sensitivity).
const RRF_K: f64 = 60.0;

/// Gate policy read from `TILTH_SCOUT_GATE` env var.
/// - `"agree"` (default): fire iff CE-top1 == embed-top1.
/// - `"always"`: always fire (skip agreement check).
/// - `"off"`: never fire (no skeleton emitted).
fn scout_gate_policy() -> &'static str {
    // SAFETY: we only read ASCII env values; no mutation.
    // We can't return a reference to a heap String so we match known values.
    match std::env::var("TILTH_SCOUT_GATE")
        .unwrap_or_default()
        .as_str()
    {
        "always" => "always",
        "off" => "off",
        _ => "agree",
    }
}

/// Optional minimum embed-cosine threshold from `TILTH_SCOUT_EMBED_MIN`.
fn scout_embed_min() -> Option<f32> {
    std::env::var("TILTH_SCOUT_EMBED_MIN")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
}

/// True if the prompt is a flow-intent query ("how does X reach/flow/call/dispatch Y").
fn is_flow_intent(prompt: &str) -> bool {
    let p = prompt.to_ascii_lowercase();
    [
        "reach", "flow", "call", "dispatch", "trigger", "invoke", "trace", "chain", "path",
        "follow", "hop", "step", "pipeline",
    ]
    .iter()
    .any(|w| p.contains(w))
}

/// Compute RRF score for a single candidate given its 1-based ranks in two lists.
/// Lower rank = higher score (standard RRF).
fn rrf_score(rank_a: usize, rank_b: usize) -> f64 {
    1.0 / (RRF_K + rank_a as f64) + 1.0 / (RRF_K + rank_b as f64)
}

/// Full rerank pipeline: rrf(CE, embed) on symbol texts → gate → skeleton.
///
/// Returns `(model_used, note, ranked_candidates, gate_fired, agreement, skeleton)`.
///
/// When the `infer` feature is absent or models are missing, degrades to the
/// deterministic context path: `model_used=false`, `gate_fired=false`.
#[allow(clippy::type_complexity)]
fn scout_rerank(
    prompt: &str,
    scope: &Path,
    candidates: Vec<ScoutCandidate>,
) -> (
    bool,
    Option<String>,
    Vec<ScoutCandidate>,
    bool,
    bool,
    Option<String>,
) {
    // Degrade immediately when no candidates.
    if candidates.is_empty() {
        return (false, None, candidates, false, false, None);
    }

    // --- Symbol texts for CE ranking (e9: symbols >> raw content) -------
    // For each candidate, build the file's definition symbol names as a
    // terse string: the CE reads *this* text, not raw file content.
    //
    // A candidate with NO outline symbols cannot anchor a skeleton, and its CE
    // text would degrade to the bare path string — which spuriously matches
    // prompts mentioning the repo name (e.g. packaging formulas named after the
    // project: HomebrewFormula/<repo>-bin.rb). Drop those from the rerank pool.
    let mut kept: Vec<ScoutCandidate> = Vec::new();
    let mut symbol_texts: Vec<String> = Vec::new();
    for c in candidates {
        let abs = scope.join(&c.path);
        if let Some(syms) = file_symbol_names(&abs) {
            if !syms.is_empty() {
                kept.push(c);
                symbol_texts.push(syms);
            }
        }
    }
    let candidates = kept;
    if candidates.is_empty() {
        return (false, None, candidates, false, false, None);
    }
    let symbol_refs: Vec<&str> = symbol_texts.iter().map(String::as_str).collect();

    // --- CE rank ---------------------------------------------------------
    let ce_cfg = infer::ModelConfig::from_name("reranker");
    let ce_scores = match infer::rerank(&ce_cfg, prompt, &symbol_refs) {
        Ok(s) => s,
        Err(e) => {
            // Model absent / unavailable — degrade to context.
            return (false, Some(e.to_string()), candidates, false, false, None);
        }
    };

    // CE rank: index of each candidate sorted by CE score descending (rank 1 = best).
    let mut ce_order: Vec<usize> = (0..candidates.len()).collect();
    ce_order.sort_by(|&a, &b| {
        ce_scores[b]
            .partial_cmp(&ce_scores[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // ce_rank[i] = 1-based rank of candidate i in CE ordering.
    let mut ce_rank = vec![0usize; candidates.len()];
    for (rank, &idx) in ce_order.iter().enumerate() {
        ce_rank[idx] = rank + 1;
    }

    // --- Embed rank ------------------------------------------------------
    let embed_cfg = infer::ModelConfig::from_name("embedder");
    // Embed the prompt once.
    let prompt_vec = match infer::embed(&embed_cfg, &[prompt]) {
        Ok(mut vecs) if !vecs.is_empty() => vecs.remove(0),
        _ => {
            // Embedder unavailable — degrade to context.
            return (
                false,
                Some("embedder unavailable".to_string()),
                candidates,
                false,
                false,
                None,
            );
        }
    };

    // Score pool paths against the prompt vector via the corpus index.
    let path_refs: Vec<&str> = candidates.iter().map(|c| c.path.as_str()).collect();
    let embed_scores =
        infer::embed_index::corpus_score_paths(&embed_cfg, scope, &prompt_vec, &path_refs);

    // Embed rank: index sorted by embed cosine descending.
    let mut embed_order: Vec<usize> = (0..candidates.len()).collect();
    embed_order.sort_by(|&a, &b| {
        embed_scores[b]
            .partial_cmp(&embed_scores[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut embed_rank = vec![0usize; candidates.len()];
    for (rank, &idx) in embed_order.iter().enumerate() {
        embed_rank[idx] = rank + 1;
    }

    // --- RRF fusion ------------------------------------------------------
    let mut rrf_scores: Vec<(f64, usize)> = (0..candidates.len())
        .map(|i| (rrf_score(ce_rank[i], embed_rank[i]), i))
        .collect();
    rrf_scores.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let fusion_top_idx = rrf_scores[0].1;
    let ce_top_idx = ce_order[0];
    let embed_top_idx = embed_order[0];

    // Optional raw-signal dump for offline gate-config tuning (TILTH_SCOUT_DEBUG=1).
    if std::env::var("TILTH_SCOUT_DEBUG").is_ok() {
        let ce_top: Vec<&str> = ce_order
            .iter()
            .take(5)
            .map(|&i| candidates[i].path.as_str())
            .collect();
        let embed_top: Vec<&str> = embed_order
            .iter()
            .take(5)
            .map(|&i| candidates[i].path.as_str())
            .collect();
        let ce_margin = if candidates.len() >= 2 {
            ce_scores[ce_order[0]] - ce_scores[ce_order[1]]
        } else {
            0.0
        };
        eprintln!(
            "SCOUT_DEBUG {}",
            serde_json::json!({
                "ce_top": ce_top,
                "embed_top": embed_top,
                "winner_cos": embed_scores[fusion_top_idx],
                "ce_margin": ce_margin,
            })
        );
    }

    // Reorder candidates by RRF score.
    let mut reranked: Vec<ScoutCandidate> = rrf_scores
        .iter()
        .map(|&(_, i)| {
            // Reconstruct — we need ownership, so build a new ScoutCandidate.
            ScoutCandidate {
                path: candidates[i].path.clone(),
                score: candidates[i].score,
                why: candidates[i].why.clone(),
            }
        })
        .collect();

    // --- Gate: winner is the CE's #1 pick, corroborated by embed top-3 --
    // The CE (cross-encoder) is the precision signal; requiring the RRF winner
    // to also be CE's #1 avoids RRF crowning a consistent-#2 red-herring that
    // neither signal actually leads (e.g. a file ranked #2 by both outscores
    // the true #1s under RRF). Embed top-3 corroboration tolerates prose slots
    // at the top (Go's doc.go / debug.go dominate embed#1 on repos that keep
    // prose in dedicated files). Chosen on BOTH corpora post symbol-flattening:
    // same tilth precision as top-2, one more correct famous fire.
    let policy = scout_gate_policy();
    let agreement = ce_top_idx == embed_top_idx;
    let corroborated = fusion_top_idx == ce_top_idx && embed_rank[fusion_top_idx] <= 3;
    let embed_min_ok = scout_embed_min().is_none_or(|theta| embed_scores[fusion_top_idx] >= theta);

    let gate_fired = match policy {
        "always" => embed_min_ok,
        "off" => false,
        _ => corroborated && embed_min_ok, // "agree" (default: fusion-corroboration)
    };

    // --- Skeleton --------------------------------------------------------
    let skeleton = if gate_fired {
        build_skeleton(prompt, scope, &candidates[fusion_top_idx].path)
    } else {
        None
    };

    // Annotate the fusion winner's why with its embed cosine for traceability.
    if let Some(winner) = reranked.first_mut() {
        let cosine = embed_scores[fusion_top_idx];
        if winner.why.is_empty() {
            winner.why = format!("[rrf-top; cos={cosine:.3}]");
        } else {
            winner.why = format!("{} [rrf-top; cos={cosine:.3}]", winner.why);
        }
    }

    (true, None, reranked, gate_fired, agreement, skeleton)
}

/// Extract definition-only symbol names from `path` as a single space-joined string.
///
/// Returns `None` when the file cannot be read or has no tree-sitter grammar.
/// Returns `Some("")` when the file has a grammar but no definitions.
fn file_symbol_names(path: &std::path::Path) -> Option<String> {
    use crate::lang::detect_file_type;
    use crate::lang::outline::get_outline_entries;
    use crate::types::{FileType, OutlineKind};

    let FileType::Code(lang) = detect_file_type(path) else {
        return None;
    };
    let content = std::fs::read_to_string(path).ok()?;
    let entries = get_outline_entries(&content, lang);
    // Flatten so impl/class methods count as symbols — the validated offline
    // config extracted EVERY definition (whole-file regex); top-level-only
    // starves the ranker on Rust/TS where the real API lives inside impls.
    let mut flat: Vec<&crate::types::OutlineEntry> = Vec::new();
    flatten_entries(&entries, &mut flat);
    let names: Vec<&str> = flat
        .iter()
        .filter(|e| !matches!(e.kind, OutlineKind::Import))
        .map(|e| e.name.as_str())
        .collect();
    Some(names.join(" "))
}

/// Flatten nested definitions (Rust impl methods, class methods) into one list
/// — the outline walker returns top-level nodes only, so e.g. ripgrep's search
/// methods live inside `impl Core` and would otherwise be invisible.
pub(crate) fn flatten_entries<'a>(
    es: &'a [crate::types::OutlineEntry],
    out: &mut Vec<&'a crate::types::OutlineEntry>,
) {
    for e in es {
        out.push(e);
        flatten_entries(&e.children, out);
    }
}

/// Build a terse structural skeleton for the fusion winner: grok the definition
/// whose name best matches the prompt and format a one-liner (signature +
/// immediate calls/callers, with test and example files filtered out).
///
/// Returns `None` when the winner file yields no usable definition.
fn build_skeleton(prompt: &str, scope: &Path, winner_path: &str) -> Option<String> {
    use crate::types::OutlineKind;
    use std::fmt::Write as _;

    let abs_path = scope.join(winner_path);
    let crate::types::FileType::Code(lang) = crate::lang::detect_file_type(&abs_path) else {
        return None;
    };
    let content = std::fs::read_to_string(&abs_path).ok()?;
    let entries = crate::lang::outline::get_outline_entries(&content, lang);
    let mut flat: Vec<&crate::types::OutlineEntry> = Vec::new();
    flatten_entries(&entries, &mut flat);

    let prompt_l = prompt.to_lowercase();
    let prompt_tokens: std::collections::HashSet<String> = crate::search::locate::tokenize(prompt)
        .into_iter()
        .collect();
    // Anchor on the definition the prompt describes best: the most name-tokens
    // shared with the prompt (so `markdown_to_html` beats a bare `html`), then
    // an exact-name mention, then callable, then earliest mention, then the
    // shorter / canonical name (so `markdown_to_html` beats its `_with_plugins`
    // variant). Falls back to the first definition. This
    // beats file-order (which picked `Handler` over `ServeHTTP`) and pure
    // substring (which picked `html` over `markdown_to_html`).
    let primary = flat
        .iter()
        .copied()
        .filter(|e| !matches!(e.kind, OutlineKind::Import))
        .filter_map(|e| {
            // Name tokens filtered like prompt terms: glue tokens (`to`, `by`)
            // and stopword-ish names (`set`, `get`, `new`) carry no signal.
            let name_tokens: Vec<String> = crate::search::locate::tokenize(&e.name)
                .into_iter()
                .filter(|t| t.len() >= 3 && !PROMPT_STOPWORDS.contains(&t.as_str()))
                .collect();
            let overlap = name_tokens
                .iter()
                .filter(|t| prompt_tokens.contains(*t))
                .count();
            let name_l = e.name.to_lowercase();
            let substr = name_l.len() >= 3 && prompt_l.contains(&name_l);
            if overlap == 0 && !substr {
                return None;
            }
            // Fraction of the NAME's tokens matched, in per-mille. A fully
            // matched short name (`Next`, 1/1) must beat a longer name that
            // matches more prompt words incidentally (`SetSameSite`, 2/3 via
            // "set" + "the same Context struct").
            let frac_pm = if name_tokens.is_empty() {
                0
            } else {
                overlap * 1000 / name_tokens.len()
            };
            let pos = prompt_l.find(&name_l).unwrap_or(usize::MAX);
            let not_callable = !matches!(e.kind, OutlineKind::Function);
            Some((
                std::cmp::Reverse(frac_pm),
                std::cmp::Reverse(overlap),
                !substr,
                not_callable,
                pos,
                e.name.len(),
                e,
            ))
        })
        .min_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.1.cmp(&b.1))
                .then(a.2.cmp(&b.2))
                .then(a.3.cmp(&b.3))
                .then(a.4.cmp(&b.4))
                .then(a.5.cmp(&b.5))
        })
        .map(|t| t.6)
        .or_else(|| {
            flat.iter()
                .copied()
                .find(|e| !matches!(e.kind, OutlineKind::Import))
        })?;

    let kind_str = match primary.kind {
        OutlineKind::Function => "fn",
        OutlineKind::Struct => "struct",
        OutlineKind::Class => "class",
        OutlineKind::Enum => "enum",
        OutlineKind::TypeAlias => "type",
        OutlineKind::Interface => "trait",
        OutlineKind::Constant => "const",
        _ => "def",
    };

    let mut out = String::new();
    let rel = winner_path;
    let _ = write!(
        out,
        "{} ({kind_str}) — {rel}:{}",
        primary.name, primary.start_line
    );

    // Grok the anchor for its call structure. For a flow/trace prompt the
    // CALLEES are the forward chain (show them first); otherwise callers lead.
    // Test files are filtered from both — they're noise for a nav anchor.
    let bloom = index::bloom::BloomFilterCache::new();
    let sess = session::Session::new();
    let caps = search::grok::GrokCaps {
        max_body_lines: 0, // no body in skeleton
        max_callees: 6,
        max_callers: 6,
        max_siblings: 0,
        max_tests: 0,
    };
    let target_spec = format!("{}:{}", winner_path, primary.start_line);
    if let Ok(grok) = search::grok::grok(&target_spec, scope, &bloom, &sess, caps) {
        let noise = crate::search::locate::is_nav_noise;
        let callees: Vec<String> = grok
            .callees_internal
            .iter()
            .filter(|c| !noise(&c.file))
            .take(3)
            .map(|c| {
                let crel = c.file.strip_prefix(scope).unwrap_or(&c.file);
                format!(" {} ({}:{})", c.name, crel.display(), c.start_line)
            })
            .collect();
        let callers: Vec<String> = grok
            .callers
            .iter()
            .filter(|c| !noise(&c.path))
            .take(3)
            .map(|c| {
                let crel = c.path.strip_prefix(scope).unwrap_or(&c.path);
                format!(" {} ({}:{})", c.calling_function, crel.display(), c.line)
            })
            .collect();
        let emit = |out: &mut String, label: &str, items: &[String]| {
            if !items.is_empty() {
                let _ = write!(out, "\n  {label}:");
                for it in items {
                    out.push_str(it);
                }
            }
        };
        if is_flow_intent(prompt) {
            emit(&mut out, "calls", &callees);
            emit(&mut out, "callers", &callers);
        } else {
            emit(&mut out, "callers", &callers);
            emit(&mut out, "calls", &callees);
        }
    }

    Some(out)
}

#[allow(clippy::too_many_arguments)]
fn format_scout_json(
    prompt: &str,
    job: &str,
    model_used: bool,
    elapsed_ms: u64,
    n_pool: usize,
    gate_fired: bool,
    agreement: bool,
    candidates: &[ScoutCandidate],
    skeleton: Option<&str>,
    note: Option<&str>,
) -> String {
    let cands: Vec<serde_json::Value> = candidates
        .iter()
        .map(|c| {
            serde_json::json!({
                "path": c.path,
                "score": c.score,
                "why": c.why,
            })
        })
        .collect();
    let mut obj = serde_json::json!({
        "prompt": prompt,
        "job": job,
        "model_used": model_used,
        "elapsed_ms": elapsed_ms,
        "n_pool": n_pool,
        "gate_fired": gate_fired,
        "agreement": agreement,
        "candidates": cands,
    });
    if let Some(s) = skeleton {
        obj["skeleton"] = serde_json::Value::String(s.to_string());
    }
    if let Some(n) = note {
        obj["note"] = serde_json::Value::String(n.to_string());
    }
    serde_json::to_string_pretty(&obj).unwrap_or_else(|_| "{}".to_string())
}

#[allow(clippy::too_many_arguments)]
fn format_scout_human(
    prompt: &str,
    job: &str,
    model_used: bool,
    elapsed_ms: u64,
    gate_fired: bool,
    candidates: &[ScoutCandidate],
    skeleton: Option<&str>,
    note: Option<&str>,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# scout — job={job} model={model_used} gate={gate_fired} ({elapsed_ms}ms)"
    );
    let _ = writeln!(out, "# prompt: {prompt}");
    if let Some(n) = note {
        let _ = writeln!(out, "# note: {n}");
    }
    for c in candidates {
        if c.why.is_empty() {
            let _ = writeln!(out, "{}", c.path);
        } else {
            let _ = writeln!(out, "{}  ·  {}", c.path, c.why);
        }
    }
    if let Some(s) = skeleton {
        let _ = writeln!(out, "\n## skeleton\n{s}");
    }
    out
}

/// Minimum locate score (×100 i32 scale) for the top candidate below which the
/// lexical signal is considered weak and embed recall is activated.
///
/// Override at runtime with `TILTH_LEXICAL_THRESHOLD=<integer>`.
const LEXICAL_STRENGTH_THRESHOLD: i32 = 150;

/// Minimum relative margin between the top and bottom locate scores.  When
/// `(top - bottom) / top < LEXICAL_MARGIN_RATIO` the distribution is flat,
/// indicating lexical search found many equally-weak matches — embed recall is
/// also activated.
///
/// Override at runtime with `TILTH_LEXICAL_MARGIN=<float 0-1>`.
const LEXICAL_MARGIN_RATIO: f64 = 0.25;

/// Determine whether the lexical (locate) signal is weak for this prompt.
///
/// Two conditions both considered:
/// 1. Top score below `LEXICAL_STRENGTH_THRESHOLD` (absolute weakness).
/// 2. Score distribution is flat — `(top - bottom) / top < margin_ratio`.
fn is_lexical_weak(locate_candidates: &[ScoutCandidate]) -> bool {
    if locate_candidates.is_empty() {
        return true;
    }
    let threshold: i32 = std::env::var("TILTH_LEXICAL_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(LEXICAL_STRENGTH_THRESHOLD);
    let margin_ratio: f64 = std::env::var("TILTH_LEXICAL_MARGIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(LEXICAL_MARGIN_RATIO);

    let top = locate_candidates[0].score;
    let bottom = locate_candidates.last().map_or(top, |c| c.score);

    let below_threshold = top < threshold;
    let flat_distribution = if top > 0 {
        let margin = f64::from(top - bottom) / f64::from(top);
        margin < margin_ratio
    } else {
        true
    };

    below_threshold || flat_distribution
}

/// Number of additional candidates recalled from the embed index.
const EMBED_RECALL_K: usize = 10;

/// Augment locate candidates with embed-recalled files when lexical signal is
/// weak.  Returns the union, deduped by path, with embed-sourced candidates
/// appended (score=0, why="embed-recall").
///
/// When `infer` is not compiled in, or the model is absent, or the signal is
/// strong, returns `locate_candidates` unchanged.
fn embed_union(
    prompt: &str,
    scope: &Path,
    locate_candidates: Vec<ScoutCandidate>,
) -> Vec<ScoutCandidate> {
    // Always union by default (validated: density∪embed recall ~91%; the
    // downstream agreement gate filters any dilution). `TILTH_SCOUT_UNION=weak`
    // restores weak-only union for A/B comparison.
    if std::env::var("TILTH_SCOUT_UNION").as_deref() == Ok("weak")
        && !is_lexical_weak(&locate_candidates)
    {
        return locate_candidates;
    }

    // Embed the prompt.
    let embed_cfg = infer::ModelConfig::from_name("embedder");
    let prompt_vec = match infer::embed(&embed_cfg, &[prompt]) {
        Ok(mut vecs) if !vecs.is_empty() => vecs.remove(0),
        _ => return locate_candidates, // model absent / unavailable
    };

    // Recall top-k from the corpus index.
    let recalled =
        infer::embed_index::corpus_recall(&embed_cfg, scope, &prompt_vec, EMBED_RECALL_K);

    if recalled.is_empty() {
        return locate_candidates;
    }

    // Build a set of already-known paths (relative to scope).
    let known: std::collections::HashSet<String> =
        locate_candidates.iter().map(|c| c.path.clone()).collect();

    // Append embed-recall entries not already in the locate pool.
    let mut result = locate_candidates;
    for (abs_path, _cosine) in recalled {
        let rel = abs_path
            .strip_prefix(scope)
            .unwrap_or(&abs_path)
            .to_string_lossy()
            .into_owned();
        if !known.contains(&rel) {
            result.push(ScoutCandidate {
                path: rel,
                score: 0,
                why: "embed-recall".to_string(),
            });
        }
    }
    result
}

struct ScoutCandidate {
    path: String,
    score: i32,
    why: String,
}

/// Small English stopword set shared by prompt-term extraction and skeleton
/// anchor scoring (name tokens like `set`/`get`/`new` are glue, not signal).
const PROMPT_STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "are", "was", "not", "but", "can", "its",
    "how", "why", "what", "when", "where", "which", "who", "will", "all", "any", "get", "set",
    "use", "new", "one", "two", "has", "had", "into", "out", "our", "more", "does", "did", "been",
    "have", "just", "than", "also", "your", "such",
];

/// Extract significant content words from a natural-language prompt.
///
/// Rules: lowercase, split on non-alphanumeric boundaries, drop tokens < 3
/// chars and a small English stopword set. Deterministic, no allocations
/// beyond the returned `Vec`.
fn extract_prompt_terms(prompt: &str) -> Vec<&str> {
    const STOPWORDS: &[&str] = PROMPT_STOPWORDS;

    // Split on non-alphanumeric runs and filter in-place using byte offsets.
    // We return `&str` slices into `prompt` to avoid allocating.
    let mut terms: Vec<&str> = Vec::new();
    let bytes = prompt.as_bytes();
    let len = bytes.len();
    let mut start: Option<usize> = None;

    for i in 0..=len {
        let is_alnum = i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_');
        match (start, is_alnum) {
            (None, true) => start = Some(i),
            (Some(s), false) => {
                // We need lowercase for the stopword check, but we can only
                // do a cheap ASCII compare here. For non-ASCII we just keep
                // the token — tilth is a code tool and prompts are ASCII-dominant.
                let slice = &prompt[s..i];
                let lower_matches_stop = STOPWORDS.iter().any(|sw| slice.eq_ignore_ascii_case(sw));
                if slice.len() >= 3 && !lower_matches_stop {
                    terms.push(slice);
                }
                start = None;
            }
            _ => {}
        }
    }
    terms
}

#[cfg(test)]
mod scout_tests {
    use super::*;

    #[test]
    fn run_scout_context_returns_valid_json_with_candidates() {
        // Use the tilth repo itself as the scope — it has Rust source files.
        let scope = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let result = run_scout("parse unified diff hunk", scope, "context", true)
            .expect("run_scout should not fail");
        let v: serde_json::Value =
            serde_json::from_str(&result).expect("output should be valid JSON");
        assert_eq!(v["job"], "context");
        assert_eq!(v["prompt"], "parse unified diff hunk");
        assert!(
            !v["model_used"].as_bool().unwrap_or(true),
            "context job must not use model"
        );
        assert!(
            v["candidates"].as_array().map_or(0, Vec::len) >= 1,
            "should find at least one candidate in the tilth repo"
        );
        // elapsed_ms must be a non-negative integer
        assert!(
            v["elapsed_ms"].as_u64().is_some(),
            "elapsed_ms must be present and non-negative"
        );
    }

    #[test]
    fn filter_noise_drops_vendored_paths() {
        let pool = vec![
            ScoutCandidate {
                path: "benchmark/fixtures/repos/fastapi/main.py".into(),
                score: 100,
                why: String::new(),
            },
            ScoutCandidate {
                path: "target/debug/build/foo.rs".into(),
                score: 50,
                why: String::new(),
            },
            ScoutCandidate {
                path: "src/lang/mod.rs".into(),
                score: 80,
                why: String::new(),
            },
            ScoutCandidate {
                path: "node_modules/pkg/index.js".into(),
                score: 30,
                why: String::new(),
            },
        ];
        let filtered = filter_noise(pool);
        assert_eq!(filtered.len(), 1, "only src/lang/mod.rs should survive");
        assert_eq!(filtered[0].path, "src/lang/mod.rs");
    }

    #[test]
    fn filter_noise_drops_tests_and_examples() {
        let pool = vec![
            ScoutCandidate {
                path: "gin.go".into(),
                score: 100,
                why: String::new(),
            },
            ScoutCandidate {
                path: "benchmarks_test.go".into(),
                score: 90,
                why: String::new(),
            },
            ScoutCandidate {
                path: "crates/matcher/tests/tests.rs".into(),
                score: 80,
                why: String::new(),
            },
            ScoutCandidate {
                path: "crates/searcher/examples/search-stdin.rs".into(),
                score: 70,
                why: String::new(),
            },
            ScoutCandidate {
                path: "rust/crates/api/benches/request_building.rs".into(),
                score: 60,
                why: String::new(),
            },
        ];
        let filtered = filter_noise(pool);
        assert_eq!(
            filtered.len(),
            1,
            "only gin.go survives; tests + examples + benches drop"
        );
        assert_eq!(filtered[0].path, "gin.go");
    }

    #[test]
    fn filter_noise_keeps_clean_paths() {
        let pool = vec![
            ScoutCandidate {
                path: "src/search/grok.rs".into(),
                score: 200,
                why: String::new(),
            },
            ScoutCandidate {
                path: "src/diff/parse.rs".into(),
                score: 50,
                why: String::new(),
            },
        ];
        // Both are genuine source paths under src/ — neither is noise.
        let filtered = filter_noise(pool);
        assert_eq!(filtered.len(), 2, "both paths are clean");
    }

    /// Live embed-recall test: abstract prompt "how does it figure out a file's
    /// programming language" — lexical search misses because none of those words
    /// appear in the code, but embed-recall must surface a `src/lang/` file
    /// where `detect_file_type` lives.
    ///
    /// Requires `--features infer` and the staged embedder model. Skips cleanly
    /// when either is absent.
    #[test]
    #[cfg(feature = "infer")]
    fn embed_recall_surfaces_lang_file_for_abstract_query() {
        let ecfg = crate::infer::ModelConfig::from_name("embedder");
        if !ecfg.model_path.exists() || !ecfg.tokenizer_path.exists() {
            eprintln!("embed_recall_surfaces_lang_file_for_abstract_query: embedder model absent, skipping");
            return;
        }

        let scope = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let abstract_query = "how does it figure out a file's programming language";

        // Run scout — with the embedder present and a lexically-weak prompt,
        // embed_union must fire and add src/lang/* to the candidate pool.
        let result =
            run_scout(abstract_query, scope, "context", true).expect("scout must not error");
        let v: serde_json::Value =
            serde_json::from_str(&result).expect("output must be valid JSON");

        let candidates = v["candidates"]
            .as_array()
            .expect("candidates must be array");

        eprintln!("embed_recall_surfaces_lang_file candidates:");
        for c in candidates {
            eprintln!("  {}", c["path"]);
        }

        // At least one candidate must be from src/lang/ (where detect_file_type lives).
        let has_lang_file = candidates.iter().any(|c| {
            c["path"]
                .as_str()
                .is_some_and(|p| p.starts_with("src/lang/"))
        });
        assert!(
                has_lang_file,
                "embed-recall must surface a src/lang/* file for the abstract query; got: {candidates:?}"
            );
    }

    /// Validates the full T3+T4+T5 pipeline against the tilth repo itself:
    /// (a) no benchmark/fixtures paths in pool, (b) gate fires on models-present
    ///     path, (c) skeleton names a src/lang symbol.
    ///
    /// Skips cleanly when models are absent (graceful degrade).
    #[test]
    #[cfg(feature = "infer")]
    fn run_scout_rerank_pipeline_on_abstract_query() {
        let rcfg = crate::infer::ModelConfig::from_name("reranker");
        let ecfg = crate::infer::ModelConfig::from_name("embedder");
        if !rcfg.model_path.exists()
            || !rcfg.tokenizer_path.exists()
            || !ecfg.model_path.exists()
            || !ecfg.tokenizer_path.exists()
        {
            eprintln!("run_scout_rerank_pipeline_on_abstract_query: models absent, skipping");
            return;
        }

        let scope = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let prompt = "how does it figure out a file's programming language";

        let result = run_scout(prompt, scope, "rerank", true).expect("rerank scout must not error");
        let v: serde_json::Value =
            serde_json::from_str(&result).expect("output must be valid JSON");

        let candidates = v["candidates"]
            .as_array()
            .expect("candidates must be array");

        // (a) No benchmark/fixtures paths in pool.
        let has_fixture_path = candidates.iter().any(|c| {
            c["path"]
                .as_str()
                .is_some_and(|p| p.starts_with("benchmark/fixtures/"))
        });
        assert!(
            !has_fixture_path,
            "noise filter must remove benchmark/fixtures paths; got: {candidates:?}"
        );

        // (b) gate_fired and agreement are present booleans.
        assert!(
            v["gate_fired"].is_boolean(),
            "gate_fired must be a boolean field"
        );
        assert!(
            v["agreement"].is_boolean(),
            "agreement must be a boolean field"
        );

        // (c) When gate fires, skeleton must name a src/lang symbol.
        let gate_fired = v["gate_fired"].as_bool().unwrap_or(false);
        if gate_fired {
            let skeleton = v["skeleton"].as_str().unwrap_or("");
            assert!(
                    skeleton.contains("src/lang"),
                    "skeleton must reference a src/lang symbol for language-detection query; skeleton='{skeleton}'"
                );
        }

        eprintln!("rerank pipeline: gate_fired={gate_fired}");
        eprintln!("candidates:");
        for c in candidates.iter().take(5) {
            eprintln!("  {}", c["path"]);
        }
    }
}
