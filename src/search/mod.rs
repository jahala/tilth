pub mod blast;
pub mod callees;
pub mod callers;
pub mod content;
pub mod deps;
pub mod facets;
pub mod glob;
pub mod rank;
pub mod siblings;
pub mod strip;
pub mod symbol;
pub mod truncate;

use std::collections::HashSet;
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ignore::WalkBuilder;

use crate::cache::OutlineCache;
use crate::error::TilthError;
use crate::format;
use crate::read;
use crate::session::Session;
use crate::types::{estimate_tokens, FileType, Match, SearchResult};

use crate::format::rel;

// Directories that are always skipped — build artifacts, dependencies, VCS internals.
// We skip these explicitly instead of relying on .gitignore so that locally-relevant
// gitignored files (docs/, configs, generated code) are still searchable.
pub(crate) const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "__pycache__",
    ".pycache",
    "vendor",
    ".next",
    ".nuxt",
    "coverage",
    ".cache",
    ".tox",
    ".venv",
    ".eggs",
    ".mypy_cache",
    ".ruff_cache",
    ".pytest_cache",
    ".turbo",
    ".parcel-cache",
    ".svelte-kit",
    "out",
    ".output",
    ".vercel",
    ".netlify",
    ".gradle",
    ".idea",
    ".scala-build",
    "target",
    ".bloop",
    ".metals",
];

const EXPAND_FULL_FILE_THRESHOLD: u64 = 800;

/// Build a parallel directory walker that searches ALL files except known junk directories.
/// Does NOT respect .gitignore — ensures gitignored but locally-relevant files are found.
/// When `glob` is Some, applies a file-pattern override (whitelist or negation).
pub(crate) fn walker(scope: &Path, glob: Option<&str>) -> Result<ignore::WalkParallel, TilthError> {
    let threads = std::env::var("TILTH_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).clamp(2, 6))
        });

    let mut builder = WalkBuilder::new(scope);
    builder
        .follow_links(false) // Don't follow symlinks - avoids NFS/mounted volumes
        .same_file_system(true) // Stay on same device - skip cross-mount traversal
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .ignore(false)
        .parents(false)
        .threads(threads)
        .filter_entry(|entry| {
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                if let Some(name) = entry.file_name().to_str() {
                    return !SKIP_DIRS.contains(&name);
                }
            }
            true
        });

    if let Some(pattern) = glob {
        if !pattern.is_empty() {
            let mut overrides = ignore::overrides::OverrideBuilder::new(scope);
            overrides
                .add(pattern)
                .map_err(|e| TilthError::InvalidQuery {
                    query: pattern.to_string(),
                    reason: format!("invalid glob: {e}"),
                })?;
            builder.overrides(overrides.build().map_err(|e| TilthError::InvalidQuery {
                query: pattern.to_string(),
                reason: format!("invalid glob: {e}"),
            })?);
        }
    }

    Ok(builder.build_parallel())
}

/// Parse `/pattern/` regex syntax. Returns (pattern, `is_regex`).
fn parse_pattern(query: &str) -> (&str, bool) {
    if query.starts_with('/') && query.ends_with('/') && query.len() > 2 {
        (&query[1..query.len() - 1], true)
    } else {
        (query, false)
    }
}

/// Get `file_lines` estimate and mtime from metadata. One `stat()` per file.
pub(crate) fn file_metadata(path: &Path) -> (u32, SystemTime) {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let est_lines = (meta.len() / 40).max(1) as u32;
            (est_lines, mtime)
        }
        Err(_) => (0, SystemTime::UNIX_EPOCH),
    }
}

/// Dispatch search by query type.
pub fn search_symbol(
    query: &str,
    scope: &Path,
    cache: &OutlineCache,
    glob: Option<&str>,
) -> Result<String, TilthError> {
    let result = symbol::search(query, scope, None, glob)?;
    let bloom = crate::index::bloom::BloomFilterCache::new();
    format_search_result(&result, cache, None, &bloom, 0)
}

pub fn search_symbol_expanded(
    query: &str,
    scope: &Path,
    cache: &OutlineCache,
    session: &Session,
    index: &crate::index::SymbolIndex,
    bloom: &crate::index::bloom::BloomFilterCache,
    expand: usize,
    context: Option<&Path>,
    glob: Option<&str>,
) -> Result<String, TilthError> {
    // Index is available but not yet used for search fast-path.
    // Build will be triggered when the lookup path is wired in.
    let _ = index;

    let result = symbol::search(query, scope, context, glob)?;
    format_search_result(&result, cache, Some(session), bloom, expand)
}

pub fn search_multi_symbol_expanded(
    queries: &[&str],
    scope: &Path,
    cache: &OutlineCache,
    session: &Session,
    index: &crate::index::SymbolIndex,
    bloom: &crate::index::bloom::BloomFilterCache,
    expand: usize,
    context: Option<&Path>,
    glob: Option<&str>,
) -> Result<String, TilthError> {
    let _ = index; // Available but not yet used for search fast-path

    // Shared expand budget: at least 1 slot per query, or explicit expand if higher.
    // expand=0 means no expansion at all.
    let mut expand_remaining = if expand == 0 {
        0
    } else {
        expand.max(queries.len())
    };
    let mut expanded_files = HashSet::new();
    let mut sections = Vec::with_capacity(queries.len());

    for query in queries {
        let result = symbol::search(query, scope, context, glob)?;
        let mut out = format::search_header(
            &result.query,
            &result.scope,
            result.matches.len(),
            result.definitions,
            result.usages,
        );
        format_matches(
            &result.matches,
            &result.scope,
            cache,
            Some(session),
            bloom,
            &mut expand_remaining,
            &mut expanded_files,
            &mut out,
        );
        if result.total_found > result.matches.len() {
            let omitted = result.total_found - result.matches.len();
            let _ = write!(
                out,
                "\n\n... and {omitted} more matches. Narrow with scope."
            );
        }
        sections.push(out);
    }

    Ok(sections.join("\n\n---\n"))
}

pub fn search_content(
    query: &str,
    scope: &Path,
    cache: &OutlineCache,
    glob: Option<&str>,
) -> Result<String, TilthError> {
    let (pattern, is_regex) = parse_pattern(query);
    let result = content::search(pattern, scope, is_regex, None, glob)?;
    let bloom = crate::index::bloom::BloomFilterCache::new();
    format_search_result(&result, cache, None, &bloom, 0)
}

pub fn search_regex(
    pattern: &str,
    scope: &Path,
    cache: &OutlineCache,
    glob: Option<&str>,
) -> Result<String, TilthError> {
    let result = content::search(pattern, scope, true, None, glob)?;
    let bloom = crate::index::bloom::BloomFilterCache::new();
    format_search_result(&result, cache, None, &bloom, 0)
}

pub fn search_content_expanded(
    query: &str,
    scope: &Path,
    cache: &OutlineCache,
    session: &Session,
    expand: usize,
    context: Option<&Path>,
    glob: Option<&str>,
) -> Result<String, TilthError> {
    let (pattern, is_regex) = parse_pattern(query);
    let result = content::search(pattern, scope, is_regex, context, glob)?;
    let bloom = crate::index::bloom::BloomFilterCache::new();
    format_search_result(&result, cache, Some(session), &bloom, expand)
}

/// Expanded regex search — takes raw pattern, no slash wrapping needed.
pub fn search_regex_expanded(
    pattern: &str,
    scope: &Path,
    cache: &OutlineCache,
    session: &Session,
    expand: usize,
    context: Option<&Path>,
    glob: Option<&str>,
) -> Result<String, TilthError> {
    let result = content::search(pattern, scope, true, context, glob)?;
    let bloom = crate::index::bloom::BloomFilterCache::new();
    format_search_result(&result, cache, Some(session), &bloom, expand)
}

/// Raw symbol search — returns structured result for programmatic inspection.
pub fn search_symbol_raw(
    query: &str,
    scope: &Path,
    glob: Option<&str>,
) -> Result<SearchResult, TilthError> {
    symbol::search(query, scope, None, glob)
}

/// Raw content search — returns structured result for programmatic inspection.
pub fn search_content_raw(
    query: &str,
    scope: &Path,
    glob: Option<&str>,
) -> Result<SearchResult, TilthError> {
    let (pattern, is_regex) = parse_pattern(query);
    content::search(pattern, scope, is_regex, None, glob)
}

/// Raw regex search — returns structured result for programmatic inspection.
pub fn search_regex_raw(
    pattern: &str,
    scope: &Path,
    glob: Option<&str>,
) -> Result<SearchResult, TilthError> {
    content::search(pattern, scope, true, None, glob)
}

/// Format a raw search result (symbol or content — both use the same pipeline).
pub fn format_raw_result(
    result: &SearchResult,
    cache: &OutlineCache,
) -> Result<String, TilthError> {
    let bloom = crate::index::bloom::BloomFilterCache::new();
    format_search_result(result, cache, None, &bloom, 0)
}

pub fn search_glob(
    pattern: &str,
    scope: &Path,
    _cache: &OutlineCache,
) -> Result<String, TilthError> {
    let result = glob::search(pattern, scope)?;
    format_glob_result(&result, scope)
}

/// Format match entries with optional expansion.
/// Groups consecutive usage matches in the same enclosing function to reduce token noise.
/// Shared expand state enables cross-query dedup in multi-symbol search.
fn format_matches(
    matches: &[Match],
    scope: &Path,
    cache: &OutlineCache,
    session: Option<&Session>,
    bloom: &crate::index::bloom::BloomFilterCache,
    expand_remaining: &mut usize,
    expanded_files: &mut HashSet<PathBuf>,
    out: &mut String,
) {
    // Multi-file: one expand per unique file. Single-file: sequential per-match.
    // expanded_files may contain entries from prior queries (cross-query dedup).
    let multi_file = matches
        .first()
        .is_some_and(|first| matches.iter().any(|m| m.path != first.path));

    // Group consecutive non-definition matches by (path, enclosing_outline_idx).
    // Definitions are never grouped — they need individual expand with callees/siblings.
    let groups = group_matches(matches, cache);

    for group in &groups {
        if group.len() == 1 {
            // Single match — format as before
            format_single_match(
                group[0],
                scope,
                cache,
                session,
                bloom,
                expand_remaining,
                expanded_files,
                multi_file,
                out,
            );
        } else {
            // Multiple usages collapsed into one entry
            format_grouped_usages(group, scope, cache, out);
        }
    }
}

/// Group consecutive non-definition matches by (path, enclosing outline entry).
/// Dedup key for definition matches: (path, line, `def_range`, `def_name`, `impl_target`).
type DefKey<'a> = (
    &'a Path,
    u32,
    Option<(u32, u32)>,
    Option<&'a str>,
    Option<&'a str>,
);

/// Returns a Vec of groups, where each group is a slice of matches.
/// Definitions and impl matches are always singleton groups.
fn group_matches<'a>(matches: &'a [Match], cache: &OutlineCache) -> Vec<Vec<&'a Match>> {
    let mut groups: Vec<Vec<&Match>> = Vec::new();
    let mut seen_defs: HashSet<DefKey<'_>> = HashSet::new();

    for m in matches {
        if m.is_definition || m.impl_target.is_some() {
            let key = (
                m.path.as_path(),
                m.line,
                m.def_range,
                m.def_name.as_deref(),
                m.impl_target.as_deref(),
            );
            if !seen_defs.insert(key) {
                continue;
            }
        }
        // Definitions and impls are never grouped
        if m.is_definition || m.impl_target.is_some() {
            groups.push(vec![m]);
            continue;
        }

        // For usages: try to merge with previous group if same (path, outline_idx)
        if let Some(last_group) = groups.last_mut() {
            let prev = last_group[0];
            // Only merge usages (previous must also be a usage in the same file)
            if !prev.is_definition
                && prev.impl_target.is_none()
                && prev.path == m.path
                && m.file_lines >= 50
            {
                let prev_idx = find_enclosing_outline_idx(&prev.path, prev.line, cache);
                let curr_idx = find_enclosing_outline_idx(&m.path, m.line, cache);
                if prev_idx.is_some() && prev_idx == curr_idx {
                    last_group.push(m);
                    continue;
                }
            }
        }
        groups.push(vec![m]);
    }
    groups
}

/// Format a group of usages collapsed into a single entry.
fn format_grouped_usages(group: &[&Match], scope: &Path, cache: &OutlineCache, out: &mut String) {
    let first = group[0];
    let path_str = rel(&first.path, scope);

    // Build comma-separated line list, collapsing consecutive runs (e.g. 55,56,57 → 55-57)
    let lines: Vec<u32> = group.iter().map(|m| m.line).collect();
    let line_str = format_line_list(&lines);

    // Get enclosing function name from outline
    let fn_name = get_outline_str(&first.path, cache).and_then(|outline_str| {
        let outline_lines: Vec<&str> = outline_str.lines().collect();
        let idx = outline_lines.iter().position(|line| {
            extract_line_range(line).is_some_and(|(s, e)| first.line >= s && first.line <= e)
        })?;
        // Extract name: outline lines look like "  [45-79]      fn TestMiddlewareNoRoute"
        let entry = outline_lines[idx].trim();
        // Find the name after "fn " or similar keyword
        entry.split_whitespace().last().map(String::from)
    });

    let _ = write!(out, "\n\n## {path_str}:{line_str} [{} usages", group.len());
    if let Some(ref name) = fn_name {
        let _ = write!(out, " in {name}");
    }
    out.push(']');

    // Show outline context once for the group
    if let Some(context) = outline_context_for_match(&first.path, first.line, cache) {
        out.push_str(&context);
    }
}

/// Format a comma-separated line list, collapsing consecutive runs.
/// e.g. [50, 55, 56, 57, 58, 63, 67] → "50,55-58,63,67"
fn format_line_list(lines: &[u32]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::new();
    let mut run_start = lines[0];
    let mut run_end = lines[0];
    for &line in &lines[1..] {
        if line == run_end + 1 {
            run_end = line;
        } else {
            if run_end > run_start + 1 {
                parts.push(format!("{run_start}-{run_end}"));
            } else if run_end > run_start {
                parts.push(format!("{run_start},{run_end}"));
            } else {
                parts.push(format!("{run_start}"));
            }
            run_start = line;
            run_end = line;
        }
    }
    if run_end > run_start + 1 {
        parts.push(format!("{run_start}-{run_end}"));
    } else if run_end > run_start {
        parts.push(format!("{run_start},{run_end}"));
    } else {
        parts.push(format!("{run_start}"));
    }
    parts.join(",")
}

/// Format a single match entry (unchanged from original behavior).
fn format_single_match(
    m: &Match,
    scope: &Path,
    cache: &OutlineCache,
    session: Option<&Session>,
    bloom: &crate::index::bloom::BloomFilterCache,
    expand_remaining: &mut usize,
    expanded_files: &mut HashSet<PathBuf>,
    multi_file: bool,
    out: &mut String,
) {
    let kind = if m.impl_target.is_some() {
        "impl"
    } else if m.is_definition {
        "definition"
    } else {
        "usage"
    };

    // Show line range for definitions with def_range, otherwise just the line
    if m.is_definition {
        if let Some((start, end)) = m.def_range {
            let _ = write!(
                out,
                "\n\n## {}:{}-{} [{kind}]",
                rel(&m.path, scope),
                start,
                end
            );
        } else {
            let _ = write!(out, "\n\n## {}:{} [{kind}]", rel(&m.path, scope), m.line);
        }
    } else {
        let _ = write!(out, "\n\n## {}:{} [{kind}]", rel(&m.path, scope), m.line);
    }

    // Skip outline for small files — the expanded code speaks for itself
    if m.file_lines < 50 {
        let _ = write!(out, "\n→ [{}]   {}", m.line, m.text);
    } else if let Some(context) = outline_context_for_match(&m.path, m.line, cache) {
        out.push_str(&context);
    } else {
        let _ = write!(out, "\n→ [{}]   {}", m.line, m.text);
    }

    if *expand_remaining > 0 {
        // Check session dedup for definitions with def_range
        let deduped = m.is_definition
            && m.def_range.is_some()
            && session.is_some_and(|s| s.is_expanded(&m.path, m.line));

        if deduped {
            if let Some((start, end)) = m.def_range {
                let _ = write!(
                    out,
                    "\n\n[shown earlier] {}:{}-{} {}",
                    rel(&m.path, scope),
                    start,
                    end,
                    m.text
                );
            }
        } else {
            let skip = multi_file && expanded_files.contains(&m.path);
            if !skip {
                if let Some((code, content)) = expand_match(m, scope) {
                    if m.is_definition && m.def_range.is_some() {
                        if let Some(s) = session {
                            s.record_expand(&m.path, m.line);
                        }
                    }

                    let file_type = crate::lang::detect_file_type(&m.path);
                    let mut skip_lines = strip::strip_noise(&content, &m.path, m.def_range);

                    if let Some((def_start, def_end)) = m.def_range {
                        if let crate::types::FileType::Code(lang) = file_type {
                            if let Some(keep) =
                                truncate::select_diverse_lines(&content, def_start, def_end, lang)
                            {
                                let keep_set: HashSet<u32> = keep.into_iter().collect();
                                for ln in def_start..=def_end {
                                    if !keep_set.contains(&ln) {
                                        skip_lines.insert(ln);
                                    }
                                }
                            }
                        }
                    }

                    let stripped_code = if skip_lines.is_empty() {
                        code
                    } else {
                        filter_code_lines(&code, &skip_lines)
                    };

                    out.push('\n');
                    out.push_str(&stripped_code);

                    if m.is_definition && m.def_range.is_some() {
                        if let crate::types::FileType::Code(lang) = file_type {
                            let callee_names =
                                callees::extract_callee_names(&content, lang, m.def_range);
                            if !callee_names.is_empty() {
                                let mut nodes = callees::resolve_callees_transitive(
                                    &callee_names,
                                    &m.path,
                                    &content,
                                    cache,
                                    bloom,
                                    2,
                                    15,
                                );

                                if let Some(ref name) = m.def_name {
                                    nodes.retain(|n| n.callee.name != *name);
                                }
                                if nodes.len() > 8 {
                                    nodes.sort_by_key(|n| i32::from(n.callee.file == m.path));
                                    nodes.truncate(8);
                                }

                                if !nodes.is_empty() {
                                    out.push_str("\n\n\u{2500}\u{2500} calls \u{2500}\u{2500}");
                                    for n in &nodes {
                                        let c = &n.callee;
                                        let _ = write!(
                                            out,
                                            "\n  {}  {}:{}-{}",
                                            c.name,
                                            rel(&c.file, scope),
                                            c.start_line,
                                            c.end_line
                                        );
                                        if let Some(ref sig) = c.signature {
                                            let _ = write!(out, "  {sig}");
                                        }
                                        for child in &n.children {
                                            let _ = write!(
                                                out,
                                                "\n    \u{2192} {}  {}:{}-{}",
                                                child.name,
                                                rel(&child.file, scope),
                                                child.start_line,
                                                child.end_line
                                            );
                                            if let Some(ref sig) = child.signature {
                                                let _ = write!(out, "  {sig}");
                                            }
                                        }
                                    }
                                }
                            }

                            if let Some(def_range) = m.def_range {
                                let entries =
                                    crate::lang::outline::get_outline_entries(&content, lang);
                                if let Some(parent) = siblings::find_parent_entry(&entries, m.line)
                                {
                                    let refs = siblings::extract_sibling_references(
                                        &content, lang, def_range,
                                    );
                                    if !refs.is_empty() {
                                        let filtered: Vec<String> =
                                            if let Some(ref name) = m.def_name {
                                                refs.into_iter().filter(|r| r != name).collect()
                                            } else {
                                                refs
                                            };

                                        let resolved =
                                            siblings::resolve_siblings(&filtered, &parent.children);
                                        if !resolved.is_empty() {
                                            out.push_str(
                                                "\n\n\u{2500}\u{2500} siblings \u{2500}\u{2500}",
                                            );
                                            for s in &resolved {
                                                let _ = write!(
                                                    out,
                                                    "\n  {}  {}:{}-{}  {}",
                                                    s.name,
                                                    rel(&m.path, scope),
                                                    s.start_line,
                                                    s.end_line,
                                                    s.signature,
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    *expand_remaining -= 1;
                    expanded_files.insert(m.path.clone());
                }
            }
        }
    }
}

/// Format a symbol/content search result.
/// When an outline cache is available, wraps each match in the file's outline context.
/// When `expand > 0`, the top N matches inline actual code (def body or ±10 lines).
/// When there are >5 matches, groups them into facets for easier navigation.
/// Prefer source languages over their compiled equivalents.
/// Higher value = more likely to be the original source.
fn source_priority(path: &Path) -> u8 {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "ts" | "tsx" => 10,
        "rs" | "go" | "py" | "rb" | "java" | "kt" | "scala" | "swift" | "c" | "cpp" | "h"
        | "cs" | "php" => 9,
        "js" | "jsx" | "mjs" | "cjs" => 7,
        _ => 3,
    }
}

/// Find a basename-matching candidate among already-collected search matches.
fn find_basename_candidate(matches: &[Match], query_lower: &str) -> Option<PathBuf> {
    let mut candidate: Option<&Path> = None;
    let mut best_priority: u8 = 0;

    for m in matches {
        let Some(stem) = m.path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem.to_ascii_lowercase() != query_lower {
            continue;
        }
        let ext = m.path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let is_code = matches!(
            ext,
            "rs" | "ts"
                | "tsx"
                | "js"
                | "jsx"
                | "go"
                | "py"
                | "rb"
                | "java"
                | "c"
                | "cpp"
                | "h"
                | "cs"
                | "swift"
                | "kt"
                | "scala"
                | "php"
        );
        if !is_code {
            if candidate.is_none() {
                candidate = Some(&m.path);
            }
            continue;
        }
        let prio = source_priority(&m.path);
        if prio > best_priority {
            best_priority = prio;
            candidate = Some(&m.path);
        }
    }

    candidate.map(Path::to_path_buf)
}

/// Fallback: lightweight directory walk to find a basename-matching file
/// when it didn't survive ranking/truncation in the match set.
fn find_basename_fallback(scope: &Path, query_lower: &str) -> Option<PathBuf> {
    let mut candidate: Option<PathBuf> = None;
    let mut best_priority: u8 = 0;

    let walker = ignore::WalkBuilder::new(scope)
        .follow_links(true)
        .hidden(true)
        .git_ignore(true)
        .max_depth(Some(6))
        .build();

    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem.to_ascii_lowercase() != *query_lower {
            continue;
        }
        let prio = source_priority(path);
        if prio > best_priority {
            best_priority = prio;
            candidate = Some(path.to_path_buf());
        }
    }

    candidate
}

/// When a file's basename (without extension) matches the query exactly,
/// return a compact outline of that file. Helps concept queries like `cli`
/// surface the file `cli.ts` with structural context instead of scattered text matches.
///
/// Scans the already-collected search results first (fast path), falls back to
/// a lightweight directory walk when the basename file didn't survive truncation.
fn basename_file_outline(
    query: &str,
    matches: &[Match],
    scope: &Path,
    cache: &OutlineCache,
) -> Option<String> {
    let query_lower = query.to_ascii_lowercase();

    // Only trigger for short single-word queries (concept/file-level intent)
    if query_lower.is_empty() || query.contains(' ') || query.contains("::") {
        return None;
    }

    // Find the best candidate among existing matches whose basename matches the query
    let matched_path = find_basename_candidate(matches, &query_lower)
        .or_else(|| find_basename_fallback(scope, &query_lower))?;

    // Read file and generate outline
    let content = std::fs::read_to_string(&matched_path).ok()?;
    let file_type = crate::lang::detect_file_type(&matched_path);
    let mtime = std::fs::metadata(&matched_path)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

    let outline = cache.get_or_compute(&matched_path, mtime, || {
        crate::read::outline::generate(
            &matched_path,
            file_type,
            &content,
            content.as_bytes(),
            false,
        )
    });

    if outline.trim().is_empty() {
        return None;
    }

    let rel_path = rel(&matched_path, scope);
    let line_count = content.lines().count();
    Some(format!(
        "### File overview: {rel_path} ({line_count} lines)\n{outline}"
    ))
}

fn format_search_result(
    result: &SearchResult,
    cache: &OutlineCache,
    session: Option<&Session>,
    bloom: &crate::index::bloom::BloomFilterCache,
    expand: usize,
) -> Result<String, TilthError> {
    let header = format::search_header(
        &result.query,
        &result.scope,
        result.matches.len(),
        result.definitions,
        result.usages,
    );
    let mut out = header;
    let mut expand_remaining = expand;
    let mut expanded_files = HashSet::new();

    // File-level retrieval: when a file basename matches the query exactly,
    // prepend a compact outline so the agent gets file-level context first.
    if let Some(file_outline) =
        basename_file_outline(&result.query, &result.matches, &result.scope, cache)
    {
        let _ = write!(out, "\n\n{file_outline}");
    }

    // Apply faceting when there are many matches (>5)
    if result.matches.len() > 5 {
        let faceted = facets::facet_matches(result.matches.clone(), &result.scope);

        // Format each non-empty facet with section headers
        if !faceted.definitions.is_empty() {
            let _ = write!(out, "\n\n### Definitions ({})", faceted.definitions.len());
            format_matches(
                &faceted.definitions,
                &result.scope,
                cache,
                session,
                bloom,
                &mut expand_remaining,
                &mut expanded_files,
                &mut out,
            );
        }

        if !faceted.implementations.is_empty() {
            let _ = write!(
                out,
                "\n\n### Implementations ({})",
                faceted.implementations.len()
            );
            format_matches(
                &faceted.implementations,
                &result.scope,
                cache,
                session,
                bloom,
                &mut expand_remaining,
                &mut expanded_files,
                &mut out,
            );
        }

        if !faceted.tests.is_empty() {
            let _ = write!(out, "\n\n### Tests ({})", faceted.tests.len());
            // Compact test format — one line per match, no expand budget consumed
            for m in &faceted.tests {
                let _ = write!(
                    out,
                    "\n  {}:{} — {}",
                    rel(&m.path, &result.scope),
                    m.line,
                    m.text.trim()
                );
            }
        }

        if !faceted.usages_local.is_empty() {
            let _ = write!(
                out,
                "\n\n### Usages — same package ({})",
                faceted.usages_local.len()
            );
            format_matches(
                &faceted.usages_local,
                &result.scope,
                cache,
                session,
                bloom,
                &mut expand_remaining,
                &mut expanded_files,
                &mut out,
            );
        }

        if !faceted.usages_cross.is_empty() {
            let _ = write!(
                out,
                "\n\n### Usages — other ({})",
                faceted.usages_cross.len()
            );
            format_matches(
                &faceted.usages_cross,
                &result.scope,
                cache,
                session,
                bloom,
                &mut expand_remaining,
                &mut expanded_files,
                &mut out,
            );
        }
    } else {
        // Linear display for ≤5 matches
        format_matches(
            &result.matches,
            &result.scope,
            cache,
            session,
            bloom,
            &mut expand_remaining,
            &mut expanded_files,
            &mut out,
        );
    }

    if result.total_found > result.matches.len() {
        let omitted = result.total_found - result.matches.len();
        let _ = write!(
            out,
            "\n\n... and {omitted} more matches. Narrow with scope."
        );
    }

    let tokens = estimate_tokens(out.len() as u64);
    let token_str = if tokens >= 1000 {
        format!("~{}.{}k", tokens / 1000, (tokens % 1000) / 100)
    } else {
        format!("~{tokens}")
    };
    let _ = write!(out, "\n\n({token_str} tokens)");

    Ok(out)
}

/// Inline the actual code for a match. Returns `(formatted_block, raw_content)`.
/// The raw content is returned so the caller can reuse it (e.g. for related-file hints)
/// without a redundant file read.
///
/// For definitions: use tree-sitter node range (`def_range`).
/// For usages: ±10 lines around the match.
fn expand_match(m: &Match, scope: &Path) -> Option<(String, String)> {
    let content = fs::read_to_string(&m.path).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len() as u32;

    let (mut start, end) = if estimate_tokens(content.len() as u64) < EXPAND_FULL_FILE_THRESHOLD {
        (1, total)
    } else {
        let (s, e) = m
            .def_range
            .unwrap_or((m.line.saturating_sub(10), m.line.saturating_add(10)));
        (s.max(1), e.min(total))
    };

    // Skip leading import blocks in expanded definitions near top of file
    if m.is_definition && start <= 5 {
        let mut first_non_import = start;
        for i in start..=end {
            let idx = (i - 1) as usize;
            if idx >= lines.len() {
                break;
            }
            let trimmed = lines[idx].trim();
            let is_import = trimmed.starts_with("use ")
                || trimmed.starts_with("import ")
                || trimmed.starts_with("from ")
                || trimmed.starts_with("#include")
                || trimmed.starts_with("require(")
                || trimmed.starts_with("require ")
                || (trimmed.starts_with("const ") && trimmed.contains("= require("));

            if !is_import && !trimmed.is_empty() {
                first_non_import = i;
                break;
            }
        }
        // Guard: only skip if we found at least one non-import line
        if first_non_import > start && first_non_import <= end {
            start = first_non_import;
        }
    }

    let mut out = String::new();
    let _ = write!(out, "\n```{}:{}-{}", rel(&m.path, scope), start, end);

    // Track consecutive blank lines for collapsing
    let mut prev_blank = false;
    for i in start..=end {
        let idx = (i - 1) as usize;
        if idx < lines.len() {
            let line = lines[idx];
            let is_blank = line.trim().is_empty();

            // Skip consecutive blank lines (keep first, drop rest)
            if is_blank && prev_blank {
                continue;
            }

            let _ = write!(out, "\n{i:>4} │ {line}");
            prev_blank = is_blank;
        }
    }
    out.push_str("\n```");
    Some((out, content))
}

/// Filter formatted code lines using a set of line numbers to skip.
/// Input is the fenced code block from `expand_match` (opening/closing fence lines
/// plus numbered content lines). Inserts gap markers for runs of >3 skipped lines.
fn filter_code_lines(code: &str, skip_lines: &HashSet<u32>) -> String {
    let mut kept: Vec<String> = Vec::new();
    let mut consecutive_skipped: u32 = 0;

    for segment in code.split('\n') {
        // Fence lines and the leading empty segment pass through unchanged
        if segment.starts_with("```") || segment.is_empty() {
            flush_gap_marker(&mut kept, &mut consecutive_skipped);
            kept.push(segment.to_owned());
            continue;
        }

        // Extract line number from formatted line: "  42 │ content"
        let line_num = segment
            .find('│')
            .and_then(|pos| segment[..pos].trim().parse::<u32>().ok());

        if let Some(num) = line_num {
            if skip_lines.contains(&num) {
                consecutive_skipped += 1;
                continue;
            }
        }

        flush_gap_marker(&mut kept, &mut consecutive_skipped);
        kept.push(segment.to_owned());
    }

    kept.join("\n")
}

/// If >3 lines were skipped consecutively, push a gap marker and reset counter.
fn flush_gap_marker(kept: &mut Vec<String>, consecutive_skipped: &mut u32) {
    if *consecutive_skipped > 3 {
        kept.push(format!(
            "       ... ({} lines omitted)",
            *consecutive_skipped
        ));
    }
    *consecutive_skipped = 0;
}

/// Get cached outline string for a file. Returns None for non-code or huge files.
fn get_outline_str(path: &std::path::Path, cache: &OutlineCache) -> Option<std::sync::Arc<str>> {
    let file_type = crate::lang::detect_file_type(path);
    if !matches!(file_type, FileType::Code(_)) {
        return None;
    }
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    if meta.len() > 500_000 {
        return None;
    }
    Some(cache.get_or_compute(path, mtime, || {
        let content = std::fs::read_to_string(path).unwrap_or_default();
        let buf = content.as_bytes();
        read::outline::generate(path, file_type, &content, buf, false)
    }))
}

/// Find the outline entry index that encloses the given line.
fn find_enclosing_outline_idx(
    path: &std::path::Path,
    match_line: u32,
    cache: &OutlineCache,
) -> Option<usize> {
    let outline_str = get_outline_str(path, cache)?;
    let outline_lines: Vec<&str> = outline_str.lines().collect();
    outline_lines.iter().position(|line| {
        extract_line_range(line).is_some_and(|(s, e)| match_line >= s && match_line <= e)
    })
}

/// Build outline context around a match — ±2 entries around the enclosing one.
fn outline_context_for_match(
    path: &std::path::Path,
    match_line: u32,
    cache: &OutlineCache,
) -> Option<String> {
    let outline_str = get_outline_str(path, cache)?;
    let outline_lines: Vec<&str> = outline_str.lines().collect();
    if outline_lines.is_empty() {
        return None;
    }

    let match_idx = outline_lines.iter().position(|line| {
        extract_line_range(line).is_some_and(|(s, e)| match_line >= s && match_line <= e)
    })?;

    let start = match_idx.saturating_sub(2);
    let end = (match_idx + 3).min(outline_lines.len());

    let mut context = String::new();
    for (i, line) in outline_lines.iter().enumerate().take(end).skip(start) {
        if i == match_idx {
            let _ = write!(context, "\n→ {line}");
        } else {
            let _ = write!(context, "\n  {line}");
        }
    }
    Some(context)
}

/// Extract (`start_line`, `end_line`) from an outline entry like "[20-115]" or "[16]".
fn extract_line_range(line: &str) -> Option<(u32, u32)> {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') {
        return None;
    }
    let end = trimmed.find(']')?;
    let range_str = &trimmed[1..end];
    if let Some((a, b)) = range_str.split_once('-') {
        let start: u32 = a.trim().parse().ok()?;
        // Handle import ranges like "[1-]"
        let end: u32 = if b.trim().is_empty() {
            start
        } else {
            b.trim().parse().ok()?
        };
        Some((start, end))
    } else {
        let n: u32 = range_str.trim().parse().ok()?;
        Some((n, n))
    }
}

/// Format glob search results (file list with previews).
fn format_glob_result(result: &glob::GlobResult, scope: &Path) -> Result<String, TilthError> {
    let header = format!(
        "# Glob: \"{}\" in {} — {} files",
        result.pattern,
        scope.display(),
        result.files.len()
    );

    let mut out = header;
    for file in &result.files {
        let _ = write!(out, "\n  {}", rel(&file.path, scope));
        if let Some(ref preview) = file.preview {
            let _ = write!(out, "  ({preview})");
        }
    }

    if result.total_found > result.files.len() {
        let omitted = result.total_found - result.files.len();
        let _ = write!(out, "\n\n... and {omitted} more files. Narrow with scope.");
    }

    if result.files.is_empty() && !result.available_extensions.is_empty() {
        let _ = write!(
            out,
            "\n\nNo matches. Available extensions in scope: {}",
            result.available_extensions.join(", ")
        );
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Mutex;

    /// Collect all file paths from a walker into a sorted Vec.
    fn walk_paths(scope: &Path, glob: Option<&str>) -> Vec<PathBuf> {
        let w = walker(scope, glob).expect("walker failed");
        let paths: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
        w.run(|| {
            let paths = &paths;
            Box::new(move |entry| {
                if let Ok(e) = entry {
                    if e.file_type().is_some_and(|ft| ft.is_file()) {
                        paths.lock().unwrap().push(e.into_path());
                    }
                }
                ignore::WalkState::Continue
            })
        });
        let mut v = paths.into_inner().unwrap();
        v.sort();
        v
    }

    fn extensions(paths: &[PathBuf]) -> HashSet<String> {
        paths
            .iter()
            .filter_map(|p| p.extension())
            .map(|e| e.to_string_lossy().to_string())
            .collect()
    }

    // ── walker unit tests ──

    #[test]
    fn walker_none_returns_all_file_types() {
        let scope = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let all = walk_paths(&scope, None);
        let exts = extensions(&all);
        assert!(exts.contains("rs"), "expected .rs files, got {exts:?}");
        assert!(!all.is_empty());
    }

    #[test]
    fn walker_whitelist_filters_to_matching_extension() {
        let scope = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let filtered = walk_paths(&scope, Some("*.rs"));
        assert!(!filtered.is_empty(), "whitelist should find .rs files");
        for p in &filtered {
            assert_eq!(
                p.extension().and_then(|e| e.to_str()),
                Some("rs"),
                "non-.rs file leaked through whitelist: {}",
                p.display()
            );
        }
    }

    #[test]
    fn walker_negation_excludes_matching_extension() {
        let scope = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let without_rs = walk_paths(&scope, Some("!*.rs"));
        for p in &without_rs {
            assert_ne!(
                p.extension().and_then(|e| e.to_str()),
                Some("rs"),
                ".rs file leaked through negation: {}",
                p.display()
            );
        }
    }

    #[test]
    fn walker_empty_string_equals_none() {
        let scope = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let all = walk_paths(&scope, None);
        let empty = walk_paths(&scope, Some(""));
        assert_eq!(all.len(), empty.len(), "empty glob should behave like None");
    }

    #[test]
    fn walker_invalid_glob_returns_error() {
        let scope = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let result = walker(&scope, Some("[unclosed"));
        match result {
            Err(TilthError::InvalidQuery { query, reason }) => {
                assert_eq!(query, "[unclosed");
                assert!(
                    reason.contains("invalid glob"),
                    "reason should mention 'invalid glob': {reason}"
                );
            }
            Err(other) => panic!("expected InvalidQuery, got {other}"),
            Ok(_) => panic!("expected Err for invalid glob, got Ok"),
        }
    }

    #[test]
    fn walker_brace_expansion_matches_multiple_extensions() {
        let scope = Path::new(env!("CARGO_MANIFEST_DIR"));
        let filtered = walk_paths(&scope, Some("*.{rs,toml}"));
        let exts = extensions(&filtered);
        assert!(
            exts.contains("rs"),
            "brace expansion should include .rs: {exts:?}"
        );
        assert!(
            exts.contains("toml"),
            "brace expansion should include .toml: {exts:?}"
        );
        for ext in &exts {
            assert!(
                ext == "rs" || ext == "toml",
                "unexpected extension leaked: {ext}"
            );
        }
    }

    #[test]
    fn walker_whitelist_fewer_than_unfiltered() {
        // Use project root (not src/) — project root has .toml, .md, .lock etc.
        // alongside .rs files, so *.rs is guaranteed to be a strict subset.
        let scope = Path::new(env!("CARGO_MANIFEST_DIR"));
        let all = walk_paths(&scope, None);
        let rs_only = walk_paths(&scope, Some("*.rs"));
        assert!(
            rs_only.len() < all.len(),
            "whitelist ({}) should find fewer files than unfiltered ({})",
            rs_only.len(),
            all.len()
        );
    }

    #[test]
    fn walker_path_pattern_restricts_directory() {
        let scope = Path::new(env!("CARGO_MANIFEST_DIR"));
        let filtered = walk_paths(&scope, Some("src/**/*.rs"));
        assert!(!filtered.is_empty(), "path pattern should find files");
        let src_dir = scope.join("src");
        for p in &filtered {
            assert!(
                p.starts_with(&src_dir),
                "file outside src/ leaked: {}",
                p.display()
            );
        }
    }

    // ── end-to-end through search functions ──

    #[test]
    fn content_search_glob_restricts_results() {
        let scope = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let all = content::search("TilthError", &scope, false, None, None).expect("search failed");
        let rs_only = content::search("TilthError", &scope, false, None, Some("*.rs"))
            .expect("search with glob failed");
        let toml_only = content::search("TilthError", &scope, false, None, Some("*.toml"))
            .expect("search with toml glob failed");

        assert!(all.total_found > 0, "unfiltered should find TilthError");
        assert!(rs_only.total_found > 0, "*.rs should find TilthError");
        assert_eq!(
            toml_only.total_found, 0,
            "*.toml should not find TilthError in Rust source"
        );
        for m in &rs_only.matches {
            assert_eq!(
                m.path.extension().and_then(|e| e.to_str()),
                Some("rs"),
                "non-.rs match leaked: {}",
                m.path.display()
            );
        }
    }

    #[test]
    fn symbol_search_glob_restricts_results() {
        let scope = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let rs_result =
            symbol::search("walker", &scope, None, Some("*.rs")).expect("symbol search failed");
        let toml_result = symbol::search("walker", &scope, None, Some("*.toml"))
            .expect("symbol search with toml failed");

        assert!(rs_result.total_found > 0, "*.rs should find 'walker'");
        assert_eq!(
            toml_result.total_found, 0,
            "*.toml should not find 'walker'"
        );
        for m in &rs_result.matches {
            assert_eq!(
                m.path.extension().and_then(|e| e.to_str()),
                Some("rs"),
                "non-.rs match in symbol search: {}",
                m.path.display()
            );
        }
    }

    #[test]
    fn callers_search_glob_restricts_results() {
        let scope = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let bloom = crate::index::bloom::BloomFilterCache::new();
        let rs_callers =
            callers::find_callers("walker", &scope, &bloom, Some("*.rs")).expect("callers failed");
        let toml_callers = callers::find_callers("walker", &scope, &bloom, Some("*.toml"))
            .expect("callers toml failed");

        assert!(
            !rs_callers.is_empty(),
            "*.rs should find callers of 'walker'"
        );
        assert!(
            toml_callers.is_empty(),
            "*.toml should not find callers of 'walker'"
        );
        for c in &rs_callers {
            assert_eq!(
                c.path.extension().and_then(|e| e.to_str()),
                Some("rs"),
                "non-.rs caller leaked: {}",
                c.path.display()
            );
        }
    }

    #[test]
    fn walker_skips_symlinked_file() {
        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().join("real");
        std::fs::create_dir(&real_dir).unwrap();
        std::fs::write(real_dir.join("hello.rs"), "fn main() {}").unwrap();

        let link_dir = tmp.path().join("linked");
        std::fs::create_dir(&link_dir).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(real_dir.join("hello.rs"), link_dir.join("hello.rs")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(real_dir.join("hello.rs"), link_dir.join("hello.rs"))
            .unwrap();

        let paths = walk_paths(tmp.path(), None);
        let names: Vec<&str> = paths
            .iter()
            .filter_map(|p| p.file_name()?.to_str())
            .collect();
        // With follow_links(false), should find hello.rs only once (in real/, not via symlink)
        assert_eq!(
            names.iter().filter(|n| **n == "hello.rs").count(),
            1,
            "expected hello.rs only from real dir (symlinks not followed), got: {names:?}"
        );
    }

    #[test]
    fn walker_skips_symlinked_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().join("real_pkg");
        std::fs::create_dir(&real_dir).unwrap();
        std::fs::write(real_dir.join("lib.rs"), "pub fn add() {}").unwrap();
        std::fs::write(real_dir.join("util.rs"), "pub fn helper() {}").unwrap();

        // Symlink the entire directory
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_dir, tmp.path().join("deps_link")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&real_dir, tmp.path().join("deps_link")).unwrap();

        let paths = walk_paths(tmp.path(), None);
        let link_files: Vec<_> = paths
            .iter()
            .filter(|p| p.starts_with(tmp.path().join("deps_link")))
            .collect();
        // With follow_links(false), symlinked directory should not be traversed
        assert_eq!(
            link_files.len(),
            0,
            "expected 0 files via symlinked directory (not followed), got: {link_files:?}"
        );
    }

    #[test]
    fn walker_survives_symlink_cycle() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("real.rs"), "fn main() {}").unwrap();

        // Create a symlink cycle: loop -> .
        #[cfg(unix)]
        std::os::unix::fs::symlink(tmp.path(), tmp.path().join("loop")).unwrap();

        // Should complete without hanging — ignore crate detects the cycle via inode tracking
        let paths = walk_paths(tmp.path(), None);
        let names: Vec<&str> = paths
            .iter()
            .filter_map(|p| p.file_name()?.to_str())
            .collect();
        assert!(
            names.contains(&"real.rs"),
            "should find real.rs despite cycle: {names:?}"
        );
    }

    #[test]
    fn content_search_skips_symlinked_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().join("real");
        std::fs::create_dir(&real_dir).unwrap();
        std::fs::write(
            real_dir.join("api.rs"),
            "pub fn unique_symlink_test_symbol() {}",
        )
        .unwrap();

        // Symlink the directory into the search scope
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_dir, tmp.path().join("linked")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&real_dir, tmp.path().join("linked")).unwrap();

        let result =
            content::search("unique_symlink_test_symbol", tmp.path(), false, None, None).unwrap();
        // With follow_links(false), should find symbol only in real/api.rs (not via symlink)
        assert_eq!(
            result.total_found, 1,
            "expected symbol found only in real path (symlinks not followed), got {}",
            result.total_found
        );
    }
}
