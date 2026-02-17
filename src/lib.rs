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
pub(crate) mod edit;
pub mod error;
pub(crate) mod format;
pub mod index;
pub mod install;
pub mod map;
pub mod mcp;
pub(crate) mod read;
pub(crate) mod search;
pub(crate) mod session;
pub(crate) mod types;

use std::path::Path;

use cache::OutlineCache;
use classify::classify;
use error::TilthError;
use types::QueryType;

/// The single public API. Everything flows through here:
/// classify → match on query type → return formatted string.
pub fn run(
    query: &str,
    scope: &Path,
    section: Option<&str>,
    budget_tokens: Option<u64>,
    cache: &OutlineCache,
) -> Result<String, TilthError> {
    run_inner(query, scope, section, budget_tokens, false, cache)
}

/// Full variant — forces full file output, bypassing smart views.
pub fn run_full(
    query: &str,
    scope: &Path,
    section: Option<&str>,
    budget_tokens: Option<u64>,
    cache: &OutlineCache,
) -> Result<String, TilthError> {
    run_inner(query, scope, section, budget_tokens, true, cache)
}

fn run_inner(
    query: &str,
    scope: &Path,
    section: Option<&str>,
    budget_tokens: Option<u64>,
    full: bool,
    cache: &OutlineCache,
) -> Result<String, TilthError> {
    let query_type = classify(query, scope);

    let output = match query_type {
        QueryType::FilePath(path) => read::read_file(&path, section, full, cache, false)?,

        QueryType::Glob(pattern) => search::search_glob(&pattern, scope, cache)?,

        QueryType::Symbol(name) => search::search_symbol(&name, scope, cache)?,

        QueryType::Content(text) => search::search_content(&text, scope, cache)?,

        QueryType::Fallthrough(text) => {
            // Path-like query that didn't resolve. Try symbol, then content.
            // Use structured total_found check, not string matching.
            let sym_result = search::search_symbol_raw(&text, scope)?;
            if sym_result.total_found > 0 {
                search::format_symbol_result(&sym_result, cache)?
            } else {
                let content_result = search::search_content_raw(&text, scope)?;
                if content_result.total_found > 0 {
                    search::format_content_result(&content_result, cache)?
                } else {
                    let resolved = scope.join(&text);
                    return Err(TilthError::NotFound {
                        path: resolved,
                        suggestion: read::suggest_similar_file(scope, &text),
                    });
                }
            }
        }
    };

    match budget_tokens {
        Some(b) => Ok(budget::apply(&output, b)),
        None => Ok(output),
    }
}
