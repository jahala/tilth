#![warn(clippy::pedantic)]
#![allow(
    clippy::cast_possible_truncation,  // line numbers as u32, token counts — we target 64-bit
    clippy::cast_sign_loss,            // same
    clippy::cast_possible_wrap,        // u32→i32 for tree-sitter APIs
    clippy::module_name_repetitions,   // Rust naming conventions
    clippy::similar_names,             // common in parser/search code
    clippy::too_many_lines,            // one complex function (find_definitions)
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
pub mod error;
pub(crate) mod format;
pub mod index;
pub mod install;
pub(crate) mod lang;
pub mod map;
pub mod mcp;
pub mod overview;
pub(crate) mod read;
pub(crate) mod search;
pub(crate) mod session;
pub(crate) mod types;

use std::path::Path;

use cache::OutlineCache;
use classify::classify;
use error::TilthError;
use types::QueryType;

/// Holds expanded search dependencies, allocated once.
/// Avoids scattered `Option<T>` + `unwrap()` throughout dispatch.
struct ExpandedCtx {
    session: session::Session,
    sym_index: index::SymbolIndex,
    bloom: index::bloom::BloomFilterCache,
    expand: usize,
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
    run_inner(query, scope, section, budget_tokens, false, 0, glob, cache)
}

/// Full variant — forces full file output, bypassing smart views.
pub fn run_full(
    query: &str,
    scope: &Path,
    section: Option<&str>,
    budget_tokens: Option<u64>,
    glob: Option<&str>,
    cache: &OutlineCache,
) -> Result<String, TilthError> {
    run_inner(query, scope, section, budget_tokens, true, 0, glob, cache)
}

/// Run with expanded search — inline source for top N matches.
pub fn run_expanded(
    query: &str,
    scope: &Path,
    section: Option<&str>,
    budget_tokens: Option<u64>,
    full: bool,
    expand: usize,
    glob: Option<&str>,
    cache: &OutlineCache,
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
    )
}

/// Find all callers of a symbol.
pub fn run_callers(
    target: &str,
    scope: &Path,
    expand: usize,
    budget_tokens: Option<u64>,
    glob: Option<&str>,
    cache: &OutlineCache,
) -> Result<String, TilthError> {
    let session = session::Session::new();
    let bloom = index::bloom::BloomFilterCache::new();
    let expand = if expand > 0 { expand } else { 2 };
    let output = search::callers::search_callers_expanded(
        target, scope, cache, &session, &bloom, expand, None, glob,
    )?;
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
    cache: &OutlineCache,
) -> Result<String, TilthError> {
    let bloom = index::bloom::BloomFilterCache::new();
    let result = search::deps::analyze_deps(path, scope, cache, &bloom)?;
    let budget_usize = budget_tokens.map(|b| b as usize);
    Ok(search::deps::format_deps(&result, scope, budget_usize))
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
            let sym_index = index::SymbolIndex::new();
            let bloom = index::bloom::BloomFilterCache::new();
            let expand = if expand > 0 { expand } else { 2 };
            let output = search::search_multi_symbol_expanded(
                &parts, scope, cache, &session, &sym_index, &bloom, expand, None, glob,
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
        QueryType::Glob(pattern) => search::search_glob(&pattern, scope, cache)?,
        _ if use_expanded => {
            let ctx = ExpandedCtx {
                session: session::Session::new(),
                sym_index: index::SymbolIndex::new(),
                bloom: index::bloom::BloomFilterCache::new(),
                expand,
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
            &ctx.sym_index,
            &ctx.bloom,
            ctx.expand,
            None,
            glob,
        ),
        QueryType::Concept(text) if text.contains(' ') => search::search_content_expanded(
            text,
            scope,
            cache,
            &ctx.session,
            ctx.expand,
            None,
            glob,
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
            &ctx.sym_index,
            &ctx.bloom,
            ctx.expand,
            None,
            glob,
        ),
        QueryType::Content(text) => search::search_content_expanded(
            text,
            scope,
            cache,
            &ctx.session,
            ctx.expand,
            None,
            glob,
        ),
        QueryType::Regex(pattern) => search::search_regex_expanded(
            pattern,
            scope,
            cache,
            &ctx.session,
            ctx.expand,
            None,
            glob,
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
