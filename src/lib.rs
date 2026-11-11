#![forbid(unsafe_code)]

pub(crate) mod budget;
pub(crate) mod classify;
pub mod diag;
pub mod diff;
pub(crate) mod edit;
pub(crate) mod edit_parse_check;
pub(crate) mod format;
pub mod install;
pub mod map;
pub mod mcp;
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
    pub use tilth_core::types::Lang;

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

use std::path::{Path, PathBuf};

use classify::classify;
use tilth_core::cache::OutlineCache;
use tilth_core::TilthError;
use types::QueryType;

/// What a caller that refuses a file-valued scope can offer instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileScopeHint {
    /// The caller takes a `glob` filter, so it can still reach one file.
    Glob,
    /// The caller has no per-file filter; only the directory can be offered.
    Directory,
}

/// Refuse a `scope` that names a file. One rule for the MCP tools and the CLI.
///
/// Scope only ever narrows a search. A file there used to widen it instead:
/// the MCP tools fell back to the whole root with a warning, and agents treat
/// a successful response as authoritative. `raw` is the scope as the caller
/// wrote it, so the suggested directory stays in the caller's frame (a
/// relative scope keeps resolving under the same `root`). `resolved` is the
/// path to inspect. A path that does not exist passes: callers decide how to
/// treat a missing directory.
pub fn refuse_file_scope(raw: &Path, resolved: &Path, hint: FileScopeHint) -> Result<(), String> {
    if !resolved.exists() || resolved.is_dir() {
        return Ok(());
    }
    let dir = match raw.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.display().to_string(),
        _ => ".".to_string(),
    };
    let name = raw.file_name().map_or_else(
        || raw.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let fix = match hint {
        FileScopeHint::Glob => format!(
            "To search within one file, set scope to its directory (\"{dir}\") and glob to its name (\"{name}\")."
        ),
        FileScopeHint::Directory => format!("Set scope to its directory (\"{dir}\") instead."),
    };
    Err(format!(
        "scope \"{}\" is a file, and scope must be an existing directory. {fix}",
        raw.display()
    ))
}

/// A hint for a `scope` that is not an existing directory, usually a typo.
/// One rule for the CLI, which refuses the scope, and the MCP tools, which
/// warn and fall back to `root`.
///
/// `raw` is the scope as the caller wrote it and `resolved` the path that
/// was checked, as in [`refuse_file_scope`]. The hint names up to three
/// existing directories whose name is close to the scope's last component,
/// found under the nearest existing parent and up to three levels below it,
/// so `src/serach` suggests `src/search` and `read` suggests `src/read`.
/// With nothing close, it names that nearest existing parent. Suggested
/// paths stay in the caller's frame: a relative scope gets relative paths.
/// The scan runs only when that parent lies inside `project` (the CLI's
/// working directory, or the MCP call's root) or inside a checkout, and
/// neither is the filesystem root or the home directory; anywhere else, the
/// hint names the parent alone.
#[must_use]
pub fn missing_scope_hint(raw: &Path, resolved: &Path, project: &Path) -> String {
    scope_hint(raw, resolved, project, home::home_dir().as_deref())
}

/// [`missing_scope_hint`] with the home directory given.
fn scope_hint(raw: &Path, resolved: &Path, project: &Path, home: Option<&Path>) -> String {
    let Some(wanted) = resolved
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
    else {
        return String::new();
    };
    // A relative path's last parent is "", which names the current directory.
    let dir_or_cwd = |p: &Path| -> PathBuf {
        if p.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            p.to_path_buf()
        }
    };
    let mut base = resolved;
    let mut raw_base = raw;
    while !dir_or_cwd(base).is_dir() {
        let (Some(parent), Some(raw_parent)) = (base.parent(), raw_base.parent()) else {
            return String::new();
        };
        base = parent;
        raw_base = raw_parent;
    }
    let base = dir_or_cwd(base);

    // Scan for close names only inside a project: the caller's own, or the
    // checkout the scope points into. Anywhere else the nearest parent can
    // be `/` or a mount point, where listing directories is slow or, on a
    // stale network mount, blocks. A desktop MCP server often runs in `/`
    // or the home directory, which hold every path, so neither counts.
    let home = home.and_then(|h| h.canonicalize().ok());
    let scannable = |frame: &Path| frame.parent().is_some() && home.as_deref() != Some(frame);
    let in_project = base.canonicalize().is_ok_and(|base| {
        project
            .canonicalize()
            .is_ok_and(|project| scannable(&project) && base.starts_with(project))
            || in_a_checkout(&base, scannable)
    });
    let close = if in_project {
        close_directories(&base, &wanted)
    } else {
        Vec::new()
    };
    if close.is_empty() {
        return format!(
            "The nearest existing directory is \"{}\".",
            dir_or_cwd(raw_base).display()
        );
    }
    let named: Vec<String> = close
        .iter()
        .map(|rel| format!("\"{}\"", raw_base.join(rel).display()))
        .collect();
    format!("Did you mean {}?", named.join(" or "))
}

/// Is `dir` inside a checkout: below a `scannable` directory that holds
/// `.git` or a package manifest?
fn in_a_checkout(dir: &Path, scannable: impl Fn(&Path) -> bool) -> bool {
    dir.ancestors()
        .any(|d| scannable(d) && d.join(".git").exists())
        || tilth_core::lang::package_root(dir).is_some_and(scannable)
}

/// Directories under `base` (relative to it) whose name is within a small
/// edit distance of `wanted`, one edit per three characters and at least
/// one, closest and shallowest first. The search goes three levels down and
/// stops after 2,000 directories, skips the walker's `SKIP_DIRS` and hidden
/// directories, and returns at most three names.
fn close_directories(base: &Path, wanted: &str) -> Vec<PathBuf> {
    /// Directories at depth 0 (`base`) to 2 are listed, so their children
    /// reach three levels below `base`.
    const MAX_DEPTH: usize = 3;
    const MAX_DIRS: usize = 2_000;
    let limit = (wanted.chars().count() / 3).max(1);
    let mut found: Vec<(usize, usize, PathBuf)> = Vec::new();
    let mut queue = std::collections::VecDeque::from([(PathBuf::new(), 0usize)]);
    let mut visited = 0;
    while let Some((rel, depth)) = queue.pop_front() {
        if depth >= MAX_DEPTH || visited >= MAX_DIRS {
            break;
        }
        let Ok(entries) = std::fs::read_dir(base.join(&rel)) else {
            continue;
        };
        let mut children: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_string();
                let skip = name.starts_with('.')
                    || tilth_core::search::walk::SKIP_DIRS.contains(&name.as_str());
                (!skip).then(|| rel.join(name))
            })
            .collect();
        children.sort();
        for child in children {
            visited += 1;
            let name = child
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().to_lowercase());
            // Optimal string alignment counts a swapped pair (`scr` for
            // `src`) as one edit, the most common typo in a path.
            let distance = strsim::osa_distance(wanted, &name);
            if distance <= limit {
                found.push((distance, depth, child.clone()));
            }
            queue.push_back((child, depth + 1));
        }
    }
    found.sort();
    found.into_iter().take(3).map(|(_, _, p)| p).collect()
}

/// Holds expanded search dependencies, allocated once.
/// Avoids scattered `Option<T>` + `unwrap()` throughout dispatch.
struct ExpandedCtx {
    session: session::Session,
    bloom: tilth_core::BloomFilterCache,
    expand: usize,
    /// Raises the search match cap (10 → 100). Driven by the explicit `--full`
    /// flag, NOT by `full = !is_tty`. Piped invocation must preserve the
    /// concise outline — see the `piped_invocation_does_not_auto_expand`
    /// pin in `main.rs` for the larger design rule this enforces.
    full_search: bool,
    /// Caller's real token budget, threaded into `fit_to_budget` so
    /// value-based match selection engages at the caller's actual cap
    /// instead of only above `DEFAULT_BUDGET`.
    budget: Option<u64>,
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
/// Forced full-file display covers piped-stdout promotion too, so it cannot
/// gate the search match-cap bump; `cli_full` is the *parsed* `--full` flag
/// and does that on its own (see `run_expanded`'s note). This is also the
/// path `--full --expand=0` takes, so the cap has to be raised here even
/// though the only branch that expands anything is the multi-symbol one,
/// which carries its own floor of one match per symbol.
pub fn run_full(
    query: &str,
    scope: &Path,
    section: Option<&str>,
    budget_tokens: Option<u64>,
    glob: Option<&str>,
    cache: &OutlineCache,
    cli_full: bool,
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
        cli_full,
    )
}

/// Run with expanded search — inline source for top N matches.
/// `full` controls full-file display for `FilePath` queries (driven by
/// `cli.full || !is_tty`). `cli_full` is the *parsed* `--full` flag and
/// alone gates the search match-cap bump; piped invocation must not raise
/// the cap (see `piped_invocation_does_not_auto_expand` pin).
#[expect(
    clippy::too_many_arguments,
    reason = "per-call options become one Request in #307"
)]
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
    let bloom = tilth_core::BloomFilterCache::new();
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
    full: bool,
) -> Result<String, TilthError> {
    let bloom = tilth_core::BloomFilterCache::new();
    let result = tilth_core::analyze_deps(path, scope, &bloom)?;
    let budget_usize = budget_tokens.map(|b| b as usize);
    Ok(search::deps::format_deps(
        &result,
        scope,
        budget_usize,
        full,
    ))
}

/// Grok a symbol: return def + doc + callees + callers + siblings + tests in one call.
///
/// `target_spec` accepts a bare symbol name (`parse_unified_diff`), a path:line
/// pair (`src/diff/parse.rs:7`), or a `Type::method` reference.
pub fn run_grok(target_spec: &str, scope: &Path, full: bool) -> Result<String, TilthError> {
    let bloom = tilth_core::BloomFilterCache::new();
    let session = session::Session::default();
    let caps = if full {
        search::grok::GrokCaps::full()
    } else {
        search::grok::GrokCaps::default()
    };
    let result = search::grok::grok(target_spec, scope, &bloom, &session, caps)?;
    Ok(search::grok::format_grok(&result, scope))
}

#[expect(
    clippy::too_many_arguments,
    reason = "per-call options become one Request in #307"
)]
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
        let all_identifiers = parts.iter().all(|p| classify::is_symbol_name(p));
        if parts.len() > 5 && all_identifiers {
            return Err(TilthError::InvalidQuery {
                query: query.to_string(),
                reason: "multi-symbol search supports 2-5 symbols".to_string(),
            });
        }
        if parts.len() >= 2 && parts.len() <= 5 && all_identifiers {
            let session = session::Session::new();
            let bloom = tilth_core::BloomFilterCache::new();
            let expand = if expand > 0 { expand } else { 2 };
            let output = search::search_multi_symbol_expanded(
                &parts,
                scope,
                cache,
                &session,
                &bloom,
                expand,
                None,
                glob,
                cli_full,
                budget_tokens,
            )?;
            // fit_to_budget (inside search_multi_symbol_expanded) already applied
            // the real budget_tokens with value-based selection; budget::apply's
            // own fast path is a no-op once output is already under budget.
            return match budget_tokens {
                Some(b) => Ok(budget::apply(&output, b)),
                None => Ok(output),
            };
        }
    }

    // FilePath and Glob are read operations, not search — handle before expanded dispatch
    let output = match query_type {
        QueryType::FilePath(path) => {
            let mut out = read::read_file(&path, section, full, cache, false, budget_tokens)?;
            if section.is_none() && !full && read::would_outline(&path) {
                let related = tilth_core::imports::resolve_related_files(&path, cache);
                if !related.is_empty() {
                    let hints: Vec<String> = related
                        .iter()
                        .take(read::RELATED_HINT_LIMIT)
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
                bloom: tilth_core::BloomFilterCache::new(),
                expand,
                full_search: cli_full,
                budget: budget_tokens,
            };
            run_query_expanded(&query_type, scope, cache, &ctx, glob)?
        }
        _ => run_query_basic(&query_type, scope, cache, glob, cli_full)?,
    };

    // For the expanded-search branch, fit_to_budget already applied
    // budget_tokens with value-based selection — this pass is then a no-op
    // (already under budget). For FilePath/Glob (no fit_to_budget path),
    // this remains the only enforcement, unchanged from before this fix.
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
            ctx.budget,
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
            ctx.budget,
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
            ctx.budget,
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
            ctx.budget,
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
            ctx.budget,
        ),
        // FilePath/Glob never reach here (gated by use_expanded)
        QueryType::FilePath(_) | QueryType::Glob(_) => {
            unreachable!("non-search query type in expanded path")
        }
    }
}

/// Dispatch search queries in basic mode (no expansion).
/// Only called for search query types — FilePath/Glob are handled before this.
/// `full_search` is the parsed `--full` flag. It is named apart from
/// `run_inner`'s `full`, which means full-*file* display: this one raises the
/// match cap and expands nothing, which is exactly what `--full --expand=0`
/// asks for.
fn run_query_basic(
    query_type: &QueryType,
    scope: &Path,
    cache: &OutlineCache,
    glob: Option<&str>,
    full_search: bool,
) -> Result<String, TilthError> {
    match query_type {
        QueryType::Symbol(name) => search::search_symbol(name, scope, cache, glob, full_search),
        QueryType::Concept(text) if text.contains(' ') => {
            multi_word_concept_search(text, scope, cache, glob, full_search)
        }
        QueryType::Concept(text) => {
            // Single-word concept: prefer definitions, then content, then any match.
            single_query_search(text, scope, cache, true, glob, full_search)
        }
        QueryType::Content(text) => search::search_content(text, scope, cache, glob, full_search),
        QueryType::Regex(pattern) => search::search_regex(pattern, scope, cache, glob, full_search),
        QueryType::Fallthrough(text) => {
            // Accept any symbol match immediately (no definitions preference).
            single_query_search(text, scope, cache, false, glob, full_search)
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
    cache: &OutlineCache,
    prefer_definitions: bool,
    glob: Option<&str>,
    full_search: bool,
) -> Result<String, TilthError> {
    let sym_result = search::search_symbol_raw(text, scope, glob, full_search)?;
    let accept_sym = if prefer_definitions {
        sym_result.definitions > 0
    } else {
        sym_result.total_found > 0
    };

    if accept_sym {
        return search::format_raw_result(&sym_result, cache);
    }

    let content_result = search::search_content_raw(text, scope, glob, full_search)?;
    if content_result.total_found > 0 {
        return search::format_raw_result(&content_result, cache);
    }

    // For concept queries: if symbol had usages but no definitions, show those
    if prefer_definitions && sym_result.total_found > 0 {
        return search::format_raw_result(&sym_result, cache);
    }

    // Fallthrough only carries path-shaped queries (`src/foo.rs`) that name
    // no existing file, so for them "not found" is the true answer.
    if !prefer_definitions {
        return Err(TilthError::NotFound {
            path: scope.join(text),
            suggestion: read::suggest_similar_file(scope, text),
        });
    }
    no_matches(&content_result, cache, scope, text)
}

/// A word or phrase that matched nothing: the standard `0 matches` result,
/// plus a did-you-mean when a file in `scope` has a close name. This used to
/// be `TilthError::NotFound`, which printed `not found: <scope>/<query>` and
/// exited 2, as if the query had named a missing file.
fn no_matches(
    result: &types::SearchResult,
    cache: &OutlineCache,
    scope: &Path,
    name: &str,
) -> Result<String, TilthError> {
    let out = search::format_raw_result(result, cache)?;
    Ok(match read::suggest_similar_file(scope, name) {
        Some(file) => format!("{out}\n\n> A file has a similar name: {file}"),
        None => out,
    })
}

/// Multi-word concept search: exact phrase first, then relaxed word proximity.
fn multi_word_concept_search(
    text: &str,
    scope: &Path,
    cache: &OutlineCache,
    glob: Option<&str>,
    full_search: bool,
) -> Result<String, TilthError> {
    // Try exact phrase match first
    let mut content_result = search::search_content_raw(text, scope, glob, full_search)?;
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

    let mut relaxed_result = search::search_regex_raw(&relaxed, scope, glob, full_search)?;
    relaxed_result.query = text.to_string();
    if relaxed_result.total_found > 0 {
        return search::format_raw_result(&relaxed_result, cache);
    }

    let first_word = words.first().copied().unwrap_or(text);
    no_matches(&content_result, cache, scope, first_word)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Directories and missing paths pass; only an existing file is refused.
    /// A bare file name suggests "." so the hint never names an empty scope.
    #[test]
    fn refuse_file_scope_only_refuses_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("x.py");
        std::fs::write(&file, "").unwrap();
        let missing = dir.path().join("nope");

        assert!(refuse_file_scope(dir.path(), dir.path(), FileScopeHint::Glob).is_ok());
        assert!(refuse_file_scope(&missing, &missing, FileScopeHint::Glob).is_ok());

        let err = refuse_file_scope(Path::new("x.py"), &file, FileScopeHint::Directory)
            .expect_err("an existing file must be refused");
        assert_eq!(
            err,
            "scope \"x.py\" is a file, and scope must be an existing directory. \
             Set scope to its directory (\".\") instead."
        );
    }

    /// A checkout with `src/search`, `src/read` and `docs`.
    fn dir_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for d in ["src/search", "src/read", "docs"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        dir
    }

    /// A typo in the last component names the close sibling, in the frame
    /// the caller wrote the scope in.
    #[test]
    fn missing_scope_hint_names_a_close_sibling() {
        let dir = dir_tree();
        let hint = missing_scope_hint(
            Path::new("src/serach"),
            &dir.path().join("src/serach"),
            dir.path(),
        );
        assert_eq!(hint, "Did you mean \"src/search\"?");
    }

    /// A directory given without its parent is found deeper in the tree.
    #[test]
    fn missing_scope_hint_finds_the_name_deeper() {
        let dir = dir_tree();
        let hint = missing_scope_hint(Path::new("read"), &dir.path().join("read"), dir.path());
        assert_eq!(hint, "Did you mean \"src/read\"?");
    }

    /// With nothing close, the hint names the nearest directory that exists.
    #[test]
    fn missing_scope_hint_falls_back_to_the_nearest_parent() {
        let dir = dir_tree();
        let raw = dir.path().join("src/zzqqxx/deeper");
        let hint = missing_scope_hint(&raw, &raw, dir.path());
        let src = dir.path().join("src");
        assert_eq!(
            hint,
            format!("The nearest existing directory is \"{}\".", src.display())
        );
    }

    /// The close-name scan stays inside the project. A scope whose nearest
    /// existing parent is above it (`/`, `/Users`, a mount point) used to
    /// list up to 2,000 directories there, and a stale network mount can
    /// block that. Such a hint names the nearest directory and scans nothing.
    #[test]
    fn missing_scope_hint_does_not_scan_above_the_project() {
        let dir = tempfile::tempdir().expect("tempdir");
        for d in ["src/search", "docs"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        let project = dir.path().join("src");
        let raw = dir.path().join("dosc");
        let hint = missing_scope_hint(&raw, &raw, &project);
        assert_eq!(
            hint,
            format!(
                "The nearest existing directory is \"{}\".",
                dir.path().display()
            )
        );
    }

    /// A frame of `/` or the home directory, the usual working directory of
    /// a desktop MCP server, holds every path, so it let the scan run
    /// anywhere. Such a frame grants no scan.
    #[test]
    fn missing_scope_hint_does_not_scan_from_a_frame_of_root_or_home() {
        let dir = dir_tree();
        let raw = dir.path().join("dosc");
        let nearest = format!(
            "The nearest existing directory is \"{}\".",
            dir.path().display()
        );
        assert_eq!(missing_scope_hint(&raw, &raw, Path::new("/")), nearest);
        assert_eq!(
            scope_hint(&raw, &raw, dir.path(), Some(dir.path())),
            nearest
        );
        // A checkout at home is no project either.
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let elsewhere = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            scope_hint(&raw, &raw, elsewhere.path(), Some(dir.path())),
            nearest
        );
    }

    /// An absolute scope into another checkout still gets the close name.
    #[test]
    fn missing_scope_hint_scans_the_checkout_the_scope_points_into() {
        let dir = dir_tree();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let elsewhere = tempfile::tempdir().expect("tempdir");
        let raw = dir.path().join("dosc");
        let hint = missing_scope_hint(&raw, &raw, elsewhere.path());
        let docs = dir.path().join("docs");
        assert_eq!(hint, format!("Did you mean \"{}\"?", docs.display()));
    }

    /// 15 files, each carrying one instance of every query shape that reaches
    /// `run_inner` with `expand == 0`, so one fixture covers them all.
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for i in 0..15 {
            std::fs::write(
                dir.path().join(format!("file_{i:02}.rs")),
                "// tag 424242\n\
                 // 424242 marks widget, see src/widget.rs\n\
                 pub fn widget() {}\n\
                 pub fn WidgetThing() {}\n",
            )
            .expect("write fixture");
        }
        dir
    }

    /// Every query shape `run_inner` can dispatch with `expand == 0`, each
    /// labelled with the route it takes, so a dropped forwarding names itself
    /// in the failure message. "cascade" is `single_query_search`, reached
    /// from two different arms and accepting at two different stages;
    /// "phrase" is `multi_word_concept_search`, whose exact and relaxed
    /// stages call different raw searches; the multi-symbol branch leaves
    /// `run_query_basic` entirely.
    const UNEXPANDED_QUERIES: &[(&str, &str)] = &[
        ("WidgetThing", "Symbol arm -> search_symbol"),
        ("widget", "Concept arm -> cascade, symbol stage"),
        ("marks", "Concept arm -> cascade, content stage"),
        ("424242", "Content arm -> search_content"),
        ("/4242[0-9]{2}/", "Regex arm -> search_regex"),
        ("tag 424242", "phrase -> search_content_raw"),
        ("widget 424242", "phrase -> search_regex_raw"),
        ("src/widget.rs", "Fallthrough arm -> cascade"),
        ("widget,WidgetThing", "multi-symbol branch"),
    ];

    /// Every `# Search: ...` header count in the output. Each reports one
    /// result's `matches.len()` after the cap; multi-symbol emits one header
    /// per symbol, every other shape exactly one.
    fn header_match_counts(output: &str) -> Vec<usize> {
        let counts: Vec<usize> = output
            .lines()
            .filter(|line| line.starts_with("# Search:"))
            .map(|line| {
                let (_, tail) = line.rsplit_once("— ").expect("header count separator");
                tail.split_whitespace()
                    .next()
                    .expect("count token")
                    .parse()
                    .expect("numeric match count")
            })
            .collect();
        assert!(!counts.is_empty(), "no search header in output");
        counts
    }

    /// Three fixture properties are load-bearing for the coverage above and
    /// easy to break by editing the file body. Pin them here so such an edit
    /// fails loudly instead of quietly collapsing three cascade stages into
    /// one.
    #[test]
    fn fixture_keeps_the_cascade_stages_apart() {
        let dir = fixture();
        let scope = dir.path();

        // "widget" is accepted at the symbol stage, so it never falls through.
        let widget =
            search::search_symbol_raw("widget", scope, None, false).expect("symbol search");
        assert!(widget.definitions > 0, "widget must have definitions");

        // "marks" has no definition but does have content hits, so it is the
        // only query that exercises the content stage of the cascade.
        let marks = search::search_symbol_raw("marks", scope, None, false).expect("symbol search");
        assert_eq!(marks.definitions, 0, "marks must have no definition");
        let marks_text = search::search_content_raw("marks", scope, None, false).expect("content");
        assert!(marks_text.total_found > 0, "marks must have content hits");

        // "widget 424242" must not appear verbatim, or the multi-word search
        // stops at the exact-phrase stage and never reaches the relaxed regex.
        let phrase =
            search::search_content_raw("widget 424242", scope, None, false).expect("content");
        assert_eq!(phrase.total_found, 0, "the phrase must not match verbatim");
    }

    /// `--full --expand=0` lands here: `expand == 0` keeps `run_inner` off the
    /// expanded dispatch, and every entry point it reaches instead used to
    /// receive a hardcoded `false`, handing back 10 of the up-to-100 matches
    /// `--full` had asked for.
    #[test]
    fn run_full_raises_match_cap_on_every_unexpanded_path() {
        let dir = fixture();
        let cache = OutlineCache::new();
        for &(query, path) in UNEXPANDED_QUERIES {
            let out = run_full(query, dir.path(), None, None, Some("*.rs"), &cache, true)
                .unwrap_or_else(|e| panic!("{path}: {query:?} failed: {e}"));
            for shown in header_match_counts(&out) {
                assert!(
                    shown > 10,
                    "{path}: {query:?} with --full --expand=0 showed {shown} matches, want >10"
                );
            }
        }
    }

    /// The other half of the contract: `run_full` is also the piped-stdout
    /// promotion path (`full = !is_tty`, no `--full` typed), where `cli_full`
    /// is false and the cap must stay at 10. This is the
    /// `piped_invocation_does_not_auto_expand` rule one layer down, at the
    /// place the cap is actually chosen.
    #[test]
    fn run_full_keeps_default_cap_without_cli_full() {
        let dir = fixture();
        let cache = OutlineCache::new();
        for &(query, path) in UNEXPANDED_QUERIES {
            let out = run_full(query, dir.path(), None, None, Some("*.rs"), &cache, false)
                .unwrap_or_else(|e| panic!("{path}: {query:?} failed: {e}"));
            for shown in header_match_counts(&out) {
                assert!(
                    shown <= 10,
                    "{path}: {query:?} piped without --full showed {shown} matches, want at most 10"
                );
            }
        }
    }

    /// A word or phrase that matches nothing is a search with no results,
    /// not a missing path. It used to fail with `not found: <scope>/<query>`
    /// and exit 2, as if the query named a file.
    #[test]
    fn zero_hit_search_says_no_matches() {
        let dir = fixture();
        let cache = OutlineCache::new();
        for query in ["zzqqxx", "zzqq xxyy"] {
            let out = run(query, dir.path(), None, None, None, &cache)
                .unwrap_or_else(|e| panic!("{query:?} must not be an error, got: {e}"));
            assert!(
                out.starts_with(&format!("# Search: \"{query}\" in "))
                    && out.contains("— 0 matches"),
                "{query:?}: {out}"
            );
            assert!(!out.contains("not found"), "{query:?}: {out}");
        }
    }

    /// A name close to an existing file keeps its did-you-mean, now as a hint
    /// under the empty result.
    #[test]
    fn zero_hit_search_keeps_the_file_name_hint() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("config"), "x\n").unwrap();
        let cache = OutlineCache::new();
        let out = run("confg", dir.path(), None, None, None, &cache).expect("no error");
        assert!(out.contains("— 0 matches"), "{out}");
        assert!(
            out.contains("config"),
            "the hint must name the close file: {out}"
        );
    }

    /// A path-shaped query that names no file still says so: there the
    /// query really is a missing path.
    #[test]
    fn zero_hit_path_query_still_reports_the_missing_path() {
        let dir = fixture();
        let cache = OutlineCache::new();
        let err = run("src/nothing_here.rs", dir.path(), None, None, None, &cache)
            .expect_err("a missing path is an error");
        assert!(err.to_string().starts_with("not found: "), "{err}");
    }
}
