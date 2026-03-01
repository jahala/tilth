pub mod callees;
pub mod callers;
pub mod content;
pub mod facets;
pub mod glob;
pub mod rank;
pub mod siblings;
pub mod strip;
pub mod symbol;
pub mod treesitter;
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

/// Path relative to scope for cleaner output. Falls back to full path.
fn rel(path: &Path, scope: &Path) -> String {
    path.strip_prefix(scope)
        .unwrap_or(path)
        .display()
        .to_string()
}

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
    ".metals"
];

const EXPAND_FULL_FILE_THRESHOLD: u64 = 800;

/// Walk up from `path` to find the nearest package manifest (Cargo.toml,
/// package.json, go.mod, etc.). Returns the directory containing it.
pub(crate) fn package_root(path: &Path) -> Option<&Path> {
    const MANIFESTS: &[&str] = &[
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "setup.py",
        "go.mod",
        "pom.xml",
        "build.gradle",
        "build.sbt"
    ];
    let mut dir = path;
    loop {
        for m in MANIFESTS {
            if dir.join(m).exists() {
                return Some(dir);
            }
        }
        dir = dir.parent()?;
    }
}

/// Build a parallel directory walker that searches ALL files except known junk directories.
/// Does NOT respect .gitignore — ensures gitignored but locally-relevant files are found.
pub(crate) fn walker(scope: &Path) -> ignore::WalkParallel {
    WalkBuilder::new(scope)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .ignore(false)
        .parents(false)
        .filter_entry(|entry| {
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                if let Some(name) = entry.file_name().to_str() {
                    return !SKIP_DIRS.contains(&name);
                }
            }
            true
        })
        .build_parallel()
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
) -> Result<String, TilthError> {
    let result = symbol::search(query, scope, None)?;
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
) -> Result<String, TilthError> {
    // Index is available but not yet used for search fast-path.
    // Build will be triggered when the lookup path is wired in.
    let _ = index;

    let result = symbol::search(query, scope, context)?;
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
        let result = symbol::search(query, scope, context)?;
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
) -> Result<String, TilthError> {
    let (pattern, is_regex) = parse_pattern(query);
    let result = content::search(pattern, scope, is_regex, None)?;
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
) -> Result<String, TilthError> {
    let (pattern, is_regex) = parse_pattern(query);
    let result = content::search(pattern, scope, is_regex, context)?;
    let bloom = crate::index::bloom::BloomFilterCache::new();
    format_search_result(&result, cache, Some(session), &bloom, expand)
}

/// Raw symbol search — returns structured result for programmatic inspection.
pub fn search_symbol_raw(query: &str, scope: &Path) -> Result<SearchResult, TilthError> {
    symbol::search(query, scope, None)
}

/// Raw content search — returns structured result for programmatic inspection.
pub fn search_content_raw(query: &str, scope: &Path) -> Result<SearchResult, TilthError> {
    let (pattern, is_regex) = parse_pattern(query);
    content::search(pattern, scope, is_regex, None)
}

/// Format a symbol search result (public for Fallthrough path in lib.rs).
pub fn format_symbol_result(
    result: &SearchResult,
    cache: &OutlineCache,
) -> Result<String, TilthError> {
    let bloom = crate::index::bloom::BloomFilterCache::new();
    format_search_result(result, cache, None, &bloom, 0)
}

/// Format a content search result (public for Fallthrough path in lib.rs).
pub fn format_content_result(
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

/// Format match entries with optional expansion and related file hints.
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

    for m in matches {
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
                // Abbreviated: show signature + location instead of full body
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
                // Multi-file or cross-query: skip files already expanded.
                // Single-file within one query: expand sequentially (no per-file dedup).
                let skip = multi_file && expanded_files.contains(&m.path);
                if !skip {
                    if let Some((code, content)) = expand_match(m, scope) {
                        // Record expansion for future dedup
                        if m.is_definition && m.def_range.is_some() {
                            if let Some(s) = session {
                                s.record_expand(&m.path, m.line);
                            }
                        }

                        // Detect language once for truncation + callees + siblings
                        let file_type = crate::read::detect_file_type(&m.path);

                        // Strip cognitive noise (debug logs, plain comments)
                        let mut skip_lines = strip::strip_noise(&content, &m.path, m.def_range);

                        // Smart truncation: for long definitions, select diverse
                        // lines instead of showing everything
                        if let Some((def_start, def_end)) = m.def_range {
                            if let crate::types::FileType::Code(lang) = file_type {
                                if let Some(keep) = truncate::select_diverse_lines(
                                    &content, def_start, def_end, lang,
                                ) {
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
                            // Definition expansion: transitive callee resolution footer
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
                                        2,  // depth_limit
                                        15, // budget for 2nd-hop callees
                                    );

                                    // Filter out self-recursive calls (current function name)
                                    if let Some(ref name) = m.def_name {
                                        nodes.retain(|n| n.callee.name != *name);
                                    }

                                    // Cap 1st-hop at 8, prioritize cross-file over same-file
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

                                // Sibling surfacing: show referenced fields/methods
                                // from the same struct/class/impl
                                if let Some(def_range) = m.def_range {
                                    let entries = callees::get_outline_entries(&content, lang);
                                    if let Some(parent) =
                                        siblings::find_parent_entry(&entries, m.line)
                                    {
                                        let refs = siblings::extract_sibling_references(
                                            &content, lang, def_range,
                                        );
                                        if !refs.is_empty() {
                                            // Filter out the current method itself
                                            let filtered: Vec<String> =
                                                if let Some(ref name) = m.def_name {
                                                    refs.into_iter().filter(|r| r != name).collect()
                                                } else {
                                                    refs
                                                };

                                            let resolved = siblings::resolve_siblings(
                                                &filtered,
                                                &parent.children,
                                            );
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
                        } else {
                            // Usage expansion: related file hints
                            let related = crate::read::imports::resolve_related_files_with_content(
                                &m.path, &content,
                            );
                            if !related.is_empty() {
                                out.push_str("\n\n> Related: ");
                                for (i, p) in related.iter().enumerate() {
                                    if i > 0 {
                                        out.push_str(", ");
                                    }
                                    let _ = write!(out, "{}", rel(p, scope));
                                }
                            }
                        }

                        *expand_remaining -= 1;
                        // Always insert for cross-query tracking.
                        expanded_files.insert(m.path.clone());
                    }
                }
            }
        }
    }
}

/// Format a symbol/content search result.
/// When an outline cache is available, wraps each match in the file's outline context.
/// When `expand > 0`, the top N matches inline actual code (def body or ±10 lines).
/// When there are >5 matches, groups them into facets for easier navigation.
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
            format_matches(
                &faceted.tests,
                &result.scope,
                cache,
                session,
                bloom,
                &mut expand_remaining,
                &mut expanded_files,
                &mut out,
            );
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

/// Generate outline context for a search match: show nearby outline entries
/// with the matching entry highlighted using →.
fn outline_context_for_match(
    path: &std::path::Path,
    match_line: u32,
    cache: &OutlineCache,
) -> Option<String> {
    let file_type = read::detect_file_type(path);
    if !matches!(file_type, FileType::Code(_)) {
        return None;
    }

    // Get or compute the file's outline
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    let byte_len = meta.len();

    // Only compute outline context for reasonably sized files
    if byte_len > 500_000 {
        return None;
    }

    let outline_str = cache.get_or_compute(path, mtime, || {
        let content = std::fs::read_to_string(path).unwrap_or_default();
        let buf = content.as_bytes();
        read::outline::generate(path, file_type, &content, buf, false)
    });

    // Parse the outline to find entries near the match line
    let outline_lines: Vec<&str> = outline_str.lines().collect();
    if outline_lines.is_empty() {
        return None;
    }

    // Find index of the outline entry containing the match line.
    let match_idx = outline_lines.iter().position(|line| {
        extract_line_range(line).is_some_and(|(s, e)| match_line >= s && match_line <= e)
    })?;

    // Show ±2 entries around the match, clamped to bounds.
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
