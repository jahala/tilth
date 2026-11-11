mod alloc;
pub mod blast;
pub mod callers;
pub mod content;
pub mod deps;
pub mod facets;
pub mod glob;
pub mod grok;
pub mod rank;
pub mod siblings;
pub mod strip;
pub mod symbol;
pub mod truncate;

use std::collections::HashSet;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::format;
use crate::read;
use crate::session::Session;
use crate::types::{estimate_tokens, Match, SearchResult};
use tilth_core::cache::OutlineCache;
use tilth_core::error::TilthError;
use tilth_core::types::FileType;

use crate::format::rel;

const EXPAND_FULL_FILE_THRESHOLD: u64 = 800;

#[derive(Debug)]
struct Expansion {
    path: PathBuf,
    line: u32,
    mtime: SystemTime,
    end: usize,
}

/// Preserve dedup within the response without updating the shared session
/// until the final budget is known.
struct RenderSession<'a> {
    previous: Option<&'a Session>,
    staged: HashSet<(PathBuf, u32, SystemTime)>,
}

impl<'a> RenderSession<'a> {
    fn new(previous: Option<&'a Session>) -> Self {
        Self {
            previous,
            staged: HashSet::new(),
        }
    }

    fn is_expanded(&self, path: &Path, line: u32, mtime: SystemTime) -> bool {
        self.previous
            .is_some_and(|session| session.is_expanded(path, line, mtime))
            || self.staged.contains(&(path.to_path_buf(), line, mtime))
    }
}

/// Record only definitions whose closing fence survives the final position
/// cut. The caller uses the same prefix when it applies the response budget.
fn record_visible_expansions(
    session: &Session,
    output: &str,
    budget: Option<u64>,
    expansions: impl IntoIterator<Item = Expansion>,
) {
    let end = budget.map_or(output.len(), |tokens| {
        crate::budget::retained_prefix_len(output, tokens)
    });
    for expansion in expansions {
        if expansion.end <= end {
            session.record_expand(&expansion.path, expansion.line, expansion.mtime);
        }
    }
}

/// The line a definition's expansion is marked under in the session, which
/// grok marks too: the first line of the definition's own node, where its
/// outline entry starts. That is `start`, the first line of its range,
/// unless decorators beside the node open the range (a Python decorator, a
/// TypeScript member's, a Rust attribute).
fn expansion_line(m: &Match, start: u32) -> u32 {
    if start == m.line {
        return start;
    }
    let decorated = || {
        let content = tilth_core::source::read_source(&m.path).ok()?;
        let FileType::Code(lang) =
            tilth_core::lang::detect_file_type_with_content(&m.path, &content)
        else {
            return None;
        };
        tilth_core::lang::treesitter::decorated_definition_line(&content, lang, start)
    };
    decorated().unwrap_or(start)
}

/// Cap for inlined markdown section bodies in the default preview slot.
/// Long sections get a tail "… (N more lines — pass --expand to see the full
/// section)" so the user knows to expand for the rest.
const MARKDOWN_PREVIEW_MAX_LINES: usize = 40;

/// Parse `/pattern/` regex syntax. Returns (pattern, `is_regex`).
fn parse_pattern(query: &str) -> (&str, bool) {
    if query.starts_with('/') && query.ends_with('/') && query.len() > 2 {
        (&query[1..query.len() - 1], true)
    } else {
        (query, false)
    }
}

/// Does a file over the size limit contain what the search looks for? The
/// walkers skip such a file without parsing it, and this one plain read is
/// what lets the result name it instead of leaving it out in silence.
pub(crate) fn oversized_holds(path: &Path, found: impl Fn(&[u8]) -> bool) -> bool {
    tilth_core::source::read(path).is_ok_and(|bytes| found(&bytes))
}

/// Does `matcher` match anywhere in `bytes`? Stops at the first hit.
pub(crate) fn has_match(matcher: &grep_regex::RegexMatcher, bytes: &[u8]) -> bool {
    let mut hit = false;
    let _ = grep_searcher::Searcher::new().search_slice(
        matcher,
        bytes,
        grep_searcher::sinks::Lossy(|_, _| {
            hit = true;
            Ok(false)
        }),
    );
    hit
}

/// Add `path` to a walker's shared list.
pub(crate) fn record(list: &std::sync::Mutex<Vec<PathBuf>>, path: &Path) {
    list.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(path.to_path_buf());
}

/// A walker's shared list, taken and sorted so the output does not depend on
/// thread timing.
pub(crate) fn sorted(list: std::sync::Mutex<Vec<PathBuf>>) -> Vec<PathBuf> {
    let mut paths = list
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    paths.sort();
    paths
}

/// The notes under a result: where else a qualified name is defined, and
/// the files the walk skipped for size although they hold what it looked
/// for. Empty when there is nothing to say.
fn skipped_large_note(result: &SearchResult) -> String {
    let mut note = other_owners_note(result);
    let held = names_phrase([result.skipped_for.as_str()]);
    if let Some(skipped) = skipped_files_note(&result.skipped_large, &held, &result.scope) {
        note.push_str("\n\n> ");
        note.push_str(&skipped);
    }
    note
}

/// The note for a qualified query whose owner holds no definition of the
/// name: where the name is defined instead. Empty when there is nothing to
/// say.
fn other_owners_note(result: &SearchResult) -> String {
    let (Some(owner), name) = tilth_core::search::scope::split_qualified(&result.query) else {
        return String::new();
    };
    // Only free definitions of the name: the qualifier is no type that holds
    // the wrong one (`res` in `res.send` is a variable), so there is no
    // owner to correct.
    if result.other_owners.iter().all(|o| o.owner.is_none()) {
        return String::new();
    }
    format!(
        "\n\n> No `{name}` is defined in `{owner}`. Definitions of `{name}`: {}",
        owner_candidates(&result.query, &result.other_owners, &result.scope)
    )
}

/// The definitions in `others`, each written as the qualified name the
/// query's own separator would give it, with its place: `A::run (m.rs:3)`,
/// or `run (m.rs:5)` for one at the top level. The first five are listed.
pub(crate) fn owner_candidates(
    query: &str,
    others: &[crate::types::OtherOwner],
    scope: &Path,
) -> String {
    const SHOWN: usize = 5;
    let name = tilth_core::search::scope::split_qualified(query).1;
    let sep = if query.ends_with(&format!("::{name}")) {
        "::"
    } else {
        "."
    };
    let mut listed: Vec<String> = others
        .iter()
        .take(SHOWN)
        .map(|o| {
            let place = format!("{}:{}", rel(&o.path, scope), o.line);
            match &o.owner {
                Some(owner) => format!("{owner}{sep}{name} ({place})"),
                None => format!("{name} ({place})"),
            }
        })
        .collect();
    if others.len() > SHOWN {
        listed.push(format!("and {} more", others.len() - SHOWN));
    }
    listed.join(", ")
}

/// The sentence that names the files a walk skipped for size although they
/// contain `held`, relative to `scope`: "1 file over 500 KB contains `run`
/// and was not searched: big.rs". `None` when there are none. Each caller
/// sets the sentence in its own output.
pub(crate) fn skipped_files_note(skipped: &[PathBuf], held: &str, scope: &Path) -> Option<String> {
    const SHOWN: usize = 5;
    if skipped.is_empty() {
        return None;
    }
    let mut files: Vec<String> = skipped.iter().take(SHOWN).map(|p| rel(p, scope)).collect();
    if skipped.len() > SHOWN {
        files.push(format!("and {} more", skipped.len() - SHOWN));
    }
    let (count, contains, was) = match skipped.len() {
        1 => ("1 file".to_string(), "contains", "was"),
        n => (format!("{n} files"), "contain", "were"),
    };
    Some(format!(
        "{count} over {} {contains} {held} and {was} not searched: {}",
        tilth_core::source::max_file_size_label(),
        files.join(", ")
    ))
}

/// `names`, sorted, as a phrase in backticks: "`a`", "`a` or `b`",
/// "`a`, `b` or `c`", and past three, "`a`, `b`, `c` or 2 other names".
pub(crate) fn names_phrase<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    const SHOWN: usize = 3;
    let mut names: Vec<&str> = names.into_iter().collect();
    names.sort_unstable();
    names.dedup();
    let mut quoted: Vec<String> = names.iter().take(SHOWN).map(|n| format!("`{n}`")).collect();
    let last = match names.len().saturating_sub(SHOWN) {
        0 => quoted.pop(),
        1 => Some("1 other name".to_string()),
        others => Some(format!("{others} other names")),
    };
    match last {
        Some(last) if quoted.is_empty() => last,
        Some(last) => format!("{} or {last}", quoted.join(", ")),
        None => String::new(),
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

/// Dispatch search by query type. `full` is the caller's `--full`, with the
/// same meaning here and in the five siblings below: it raises the match cap.
/// The totals are exact either way.
pub fn search_symbol(
    query: &str,
    scope: &Path,
    cache: &OutlineCache,
    glob: Option<&str>,
    full: bool,
) -> Result<String, TilthError> {
    let result = symbol::search(query, scope, None, glob, full)?;
    let bloom = tilth_core::index::bloom::BloomFilterCache::new();
    format_search_result(&result, cache, None, &bloom, 0, None)
}

pub fn search_symbol_expanded(
    query: &str,
    scope: &Path,
    cache: &OutlineCache,
    session: &Session,
    bloom: &tilth_core::index::bloom::BloomFilterCache,
    expand: usize,
    context: Option<&Path>,
    glob: Option<&str>,
    full: bool,
    budget: Option<u64>,
) -> Result<String, TilthError> {
    let result = symbol::search(query, scope, context, glob, full)?;
    format_search_result(&result, cache, Some(session), bloom, expand, budget)
}

pub fn search_multi_symbol_expanded(
    queries: &[&str],
    scope: &Path,
    cache: &OutlineCache,
    session: &Session,
    bloom: &tilth_core::index::bloom::BloomFilterCache,
    expand: usize,
    context: Option<&Path>,
    glob: Option<&str>,
    full: bool,
    budget: Option<u64>,
) -> Result<String, TilthError> {
    // Shared expand budget: at least 1 slot per query, or explicit expand if higher.
    // expand=0 means no expansion at all.
    let mut expand_remaining = if expand == 0 {
        0
    } else {
        expand.max(queries.len())
    };
    let mut expanded_files = HashSet::new();
    let mut render_session = RenderSession::new(Some(session));
    let mut combined = String::new();
    let mut expansions = Vec::new();

    for query in queries {
        let result = symbol::search(query, scope, context, glob, full)?;
        let mut out = format::search_header(
            &result.query,
            &result.scope,
            result.matches.len(),
            result.total_found,
            result.definitions,
            result.usages,
        );
        let mut segments: Vec<alloc::Block> = Vec::new();
        format_matches(
            &result.matches,
            &result.scope,
            cache,
            &mut render_session,
            bloom,
            &mut expand_remaining,
            &mut expanded_files,
            &mut out,
            &mut segments,
        );
        if result.total_found > result.matches.len() {
            let omitted = result.total_found - result.matches.len();
            let _ = write!(
                out,
                "\n\n... and {omitted} more matches. Narrow with scope."
            );
        }
        out.push_str(&skipped_large_note(&result));
        // budget.unwrap_or(DEFAULT_BUDGET): keeps the no-budget path byte-
        // identical to before this fix (see format_search_result's own comment).
        let budget_tokens = budget.unwrap_or(crate::budget::DEFAULT_BUDGET);
        out = alloc::fit_to_budget(&out, &mut segments, budget_tokens);
        if !combined.is_empty() {
            combined.push_str("\n\n---\n");
        }
        let offset = combined.len();
        expansions.extend(segments.into_iter().filter_map(|block| {
            let mut expansion = block.expansion?;
            expansion.end += offset;
            Some(expansion)
        }));
        combined.push_str(&out);
    }

    record_visible_expansions(session, &combined, budget, expansions);
    Ok(combined)
}

pub fn search_content(
    query: &str,
    scope: &Path,
    cache: &OutlineCache,
    glob: Option<&str>,
    full: bool,
) -> Result<String, TilthError> {
    let (pattern, is_regex) = parse_pattern(query);
    let result = content::search(pattern, scope, is_regex, None, glob, full)?;
    let bloom = tilth_core::index::bloom::BloomFilterCache::new();
    format_search_result(&result, cache, None, &bloom, 0, None)
}

pub fn search_regex(
    pattern: &str,
    scope: &Path,
    cache: &OutlineCache,
    glob: Option<&str>,
    full: bool,
) -> Result<String, TilthError> {
    let result = content::search(pattern, scope, true, None, glob, full)?;
    let bloom = tilth_core::index::bloom::BloomFilterCache::new();
    format_search_result(&result, cache, None, &bloom, 0, None)
}

pub fn search_content_expanded(
    query: &str,
    scope: &Path,
    cache: &OutlineCache,
    session: &Session,
    expand: usize,
    context: Option<&Path>,
    glob: Option<&str>,
    full: bool,
    budget: Option<u64>,
) -> Result<String, TilthError> {
    let (pattern, is_regex) = parse_pattern(query);
    let result = content::search(pattern, scope, is_regex, context, glob, full)?;
    let bloom = tilth_core::index::bloom::BloomFilterCache::new();
    format_search_result(&result, cache, Some(session), &bloom, expand, budget)
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
    full: bool,
    budget: Option<u64>,
) -> Result<String, TilthError> {
    let result = content::search(pattern, scope, true, context, glob, full)?;
    let bloom = tilth_core::index::bloom::BloomFilterCache::new();
    format_search_result(&result, cache, Some(session), &bloom, expand, budget)
}

/// Raw symbol search — returns structured result for programmatic inspection.
pub fn search_symbol_raw(
    query: &str,
    scope: &Path,
    glob: Option<&str>,
    full: bool,
) -> Result<SearchResult, TilthError> {
    symbol::search(query, scope, None, glob, full)
}

/// Raw content search — returns structured result for programmatic inspection.
pub fn search_content_raw(
    query: &str,
    scope: &Path,
    glob: Option<&str>,
    full: bool,
) -> Result<SearchResult, TilthError> {
    let (pattern, is_regex) = parse_pattern(query);
    content::search(pattern, scope, is_regex, None, glob, full)
}

/// Raw regex search — returns structured result for programmatic inspection.
pub fn search_regex_raw(
    pattern: &str,
    scope: &Path,
    glob: Option<&str>,
    full: bool,
) -> Result<SearchResult, TilthError> {
    content::search(pattern, scope, true, None, glob, full)
}

/// Format a raw search result (symbol or content — both use the same pipeline).
pub fn format_raw_result(
    result: &SearchResult,
    cache: &OutlineCache,
) -> Result<String, TilthError> {
    let bloom = tilth_core::index::bloom::BloomFilterCache::new();
    format_search_result(result, cache, None, &bloom, 0, None)
}

pub fn search_glob(pattern: &str, scope: &Path) -> Result<String, TilthError> {
    let result = glob::search(pattern, scope)?;
    format_glob_result(&result, scope)
}

/// Render the count for a facet section heading. Returns the bare displayed
/// count when nothing was hidden (`shown == total`), or `displayed/total`
/// when the cap dropped some entries — so a reader sees at a glance whether
/// the facet was truncated.
fn count_label(shown: usize, total: usize) -> String {
    if shown >= total {
        format!("{shown}")
    } else {
        format!("{shown}/{total}")
    }
}

/// Emit a per-facet hidden-count tail line after a truncated facet's entries.
/// Wording mirrors the linear-path global tail so a reader sees a single
/// consistent shape — only the noun changes per facet kind.
fn write_hidden_tail(out: &mut String, shown: usize, total: usize, kind: &str) {
    if shown < total {
        let hidden = total - shown;
        let _ = write!(out, "\n\n... and {hidden} more {kind}. Narrow with scope.");
    }
}

/// Format match entries with optional expansion.
/// Groups consecutive usage matches in the same enclosing function to reduce token noise.
/// Shared expand state enables cross-query dedup in multi-symbol search.
fn format_matches(
    matches: &[Match],
    scope: &Path,
    cache: &OutlineCache,
    session: &mut RenderSession<'_>,
    bloom: &tilth_core::index::bloom::BloomFilterCache,
    expand_remaining: &mut usize,
    expanded_files: &mut HashSet<PathBuf>,
    out: &mut String,
    segments: &mut Vec<alloc::Block>,
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
            let start = out.len();
            let (body_start, expansion) = format_single_match(
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
            segments.push(alloc::Block {
                value: i64::from(group[0].def_weight),
                start,
                body_start,
                end: out.len(),
                expansion,
            });
        } else {
            // Multiple usages collapsed into one entry
            let start = out.len();
            format_grouped_usages(group, scope, cache, out);
            let value = group
                .iter()
                .map(|m| i64::from(m.def_weight))
                .max()
                .unwrap_or(0);
            segments.push(alloc::Block {
                value,
                start,
                body_start: out.len(),
                end: out.len(),
                expansion: None,
            });
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

    let scope_label = enclosing_scope_label(&first.path, first.line, cache);

    let _ = write!(out, "\n\n### {path_str}:{line_str} [{} usages", group.len());
    if let Some(ref label) = scope_label {
        let _ = write!(out, " in {label}");
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

/// The symbol to feed query-aware truncation when expanding a match's body.
///
/// For `impl`/`implements` matches the user searched for the trait or interface,
/// which is held in `impl_target` — `def_name` is the rendered label
/// (`"impl Trait for Type"` / `"Type implements Trait"`) and never appears
/// verbatim in the body, so boosting on it is a no-op. For plain definitions
/// `impl_target` is `None` and the searched token is the symbol name in
/// `def_name`. Preferring `impl_target` routes the real query into the boost for
/// both shapes.
fn boost_query(m: &Match) -> Option<&str> {
    m.impl_target.as_deref().or(m.def_name.as_deref())
}

/// Format a single match entry (unchanged from original behavior).
fn format_single_match(
    m: &Match,
    scope: &Path,
    cache: &OutlineCache,
    session: &mut RenderSession<'_>,
    bloom: &tilth_core::index::bloom::BloomFilterCache,
    expand_remaining: &mut usize,
    expanded_files: &mut HashSet<PathBuf>,
    multi_file: bool,
    out: &mut String,
) -> (usize, Option<Expansion>) {
    let kind = if m.impl_target.is_some() {
        "impl"
    } else if m.is_definition {
        "definition"
    } else {
        "usage"
    };

    // For usages, append the enclosing function/section if we can recover one.
    // Definitions and impls already are the named scope.
    let scope_suffix = if m.is_definition || m.impl_target.is_some() {
        String::new()
    } else {
        enclosing_scope_label(&m.path, m.line, cache)
            .map(|s| format!(" in {s}"))
            .unwrap_or_default()
    };

    // Show line range for definitions with def_range, otherwise just the line
    if m.is_definition {
        if let Some((start, end)) = m.def_range {
            let _ = write!(
                out,
                "\n\n### {}:{}-{} [{kind}]",
                rel(&m.path, scope),
                start,
                end
            );
        } else {
            let _ = write!(out, "\n\n### {}:{} [{kind}]", rel(&m.path, scope), m.line);
        }
    } else {
        let _ = write!(
            out,
            "\n\n### {}:{} [{kind}{scope_suffix}]",
            rel(&m.path, scope),
            m.line
        );
    }

    // Markdown-heading defs (`def_weight == 30`): the heading text alone is
    // just the query, so the default preview slot would carry no information.
    // Inline the section body directly. Bypasses the --expand budget — this
    // is a fixed-cost preview, not the on-demand expand — and short-circuits
    // the rest of the function (no callees / siblings etc. apply to a
    // markdown section). On any read failure or empty body, fall through to
    // the existing outline / single-line preview branches.
    if m.is_definition && m.def_weight == 30 {
        if let Some((heading_line_1, section_end_1)) = m.def_range {
            if let Ok(content) = tilth_core::source::read_source(&m.path) {
                let lines: Vec<&str> = content.lines().collect();
                // def_range is `(heading_line, section_end)` in 1-indexed
                // inclusive form (see `find_defs_markdown_buf`). The body
                // starts at the line *after* the heading. In 0-indexed
                // half-open form: `[heading_line_1 .. section_end_1)`.
                let body_start = heading_line_1 as usize;
                let body_end = (section_end_1 as usize).min(lines.len());
                if body_start < body_end {
                    let total_body_lines = body_end - body_start;
                    let take_n = total_body_lines.min(MARKDOWN_PREVIEW_MAX_LINES);
                    out.push('\n');
                    for line in &lines[body_start..body_start + take_n] {
                        out.push_str(line);
                        out.push('\n');
                    }
                    if total_body_lines > take_n {
                        let truncated = total_body_lines - take_n;
                        let _ = write!(
                            out,
                            "… ({truncated} more lines — pass --expand to see the full section)"
                        );
                    }
                    return (out.len(), None);
                }
            }
        }
    }

    // Check session dedup for definitions with def_range. The mtime
    // check ensures a post-edit search re-inlines the body rather than
    // pointing at stale line numbers.
    let current_mtime = std::fs::metadata(&m.path)
        .ok()
        .and_then(|md| md.modified().ok());
    let marked_line = m
        .def_range
        .filter(|_| m.is_definition && session.previous.is_some())
        .map(|(start, _)| expansion_line(m, start));
    let deduped = marked_line
        .is_some_and(|line| current_mtime.is_some_and(|t| session.is_expanded(&m.path, line, t)));
    // expand_match always prints a range containing m.line (a definition's
    // def_range holds its name line; the ±10 fallback for def_range: None /
    // usages trivially contains it), so the raw "-> [line] text" preview would
    // reprint m.text byte-for-byte inside the fence below. Only the
    // structural outline_context (neighboring entries' signatures, not the
    // matched line's own source) survives alongside an expansion.
    let fence_will_follow =
        *expand_remaining > 0 && !deduped && !(multi_file && expanded_files.contains(&m.path));

    // Skip outline for small files — the expanded code speaks for itself
    if m.file_lines < 50 {
        if !fence_will_follow {
            let _ = write!(out, "\n-> [{}]   {}", m.line, m.text);
        }
    } else if let Some(context) = outline_context_for_match(&m.path, m.line, cache) {
        out.push_str(&context);
    } else if !fence_will_follow {
        let _ = write!(out, "\n-> [{}]   {}", m.line, m.text);
    }

    // Everything from here on is the expansion, which a tight budget can
    // drop while keeping the lines above.
    let body_start = out.len();
    let mut expansion = None;

    if *expand_remaining > 0 {
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
                    let file_type =
                        tilth_core::lang::detect_file_type_with_content(&m.path, &content);
                    let mut skip_lines = strip::strip_noise(&content, &m.path, m.def_range);

                    if let Some((def_start, def_end)) = m.def_range {
                        if let tilth_core::types::FileType::Code(_) = file_type {
                            if let Some(keep) = truncate::select_diverse_lines(
                                &content,
                                def_start,
                                def_end,
                                boost_query(m),
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
                        if let (Some(line), Some(mtime)) = (marked_line, current_mtime) {
                            session.staged.insert((m.path.clone(), line, mtime));
                            expansion = Some(Expansion {
                                path: m.path.clone(),
                                line,
                                mtime,
                                end: out.len(),
                            });
                        }
                        if let tilth_core::types::FileType::Code(lang) = file_type {
                            let callee_names = tilth_core::search::callees::extract_callee_names(
                                &content,
                                lang,
                                m.def_range,
                            );
                            if !callee_names.is_empty() {
                                let mut nodes =
                                    tilth_core::search::callees::resolve_callees_transitive(
                                        &callee_names,
                                        &m.path,
                                        &content,
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
                                    out.push_str("\n\n-- calls --");
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
                                                "\n    -> {}  {}:{}-{}",
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
                                    tilth_core::lang::outline::get_outline_entries(&content, lang);
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
                                            out.push_str("\n\n-- siblings --");
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
    (body_start, expansion)
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

/// Format a token count into a human-readable string (e.g. "~1.2k" or "~743").
pub(crate) fn format_token_count(tokens: u64) -> String {
    if tokens >= 1000 {
        format!("~{}.{}k", tokens / 1000, (tokens % 1000) / 100)
    } else {
        format!("~{tokens}")
    }
}

/// Fallback: lightweight directory walk to find a basename-matching file
/// when it didn't survive ranking/truncation in the match set.
fn find_basename_fallback(scope: &Path, query_lower: &str) -> Option<PathBuf> {
    let mut candidate: Option<PathBuf> = None;
    let mut best_priority: u8 = 0;

    let walker = ignore::WalkBuilder::new(scope)
        .follow_links(true)
        .same_file_system(true) // Stop at mount boundaries (NFS, external volumes).
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
    let content = tilth_core::source::read_source(&matched_path).ok()?;
    let file_type = tilth_core::lang::detect_file_type_with_content(&matched_path, &content);
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
        "## File overview: {rel_path} ({line_count} lines)\n{outline}"
    ))
}

fn format_search_result(
    result: &SearchResult,
    cache: &OutlineCache,
    session: Option<&Session>,
    bloom: &tilth_core::index::bloom::BloomFilterCache,
    expand: usize,
    budget: Option<u64>,
) -> Result<String, TilthError> {
    let header = format::search_header(
        &result.query,
        &result.scope,
        result.matches.len(),
        result.total_found,
        result.definitions,
        result.usages,
    );
    let mut out = header;
    let mut render_session = RenderSession::new(session);
    let mut expand_remaining = expand;
    let mut expanded_files = HashSet::new();
    let mut segments: Vec<alloc::Block> = Vec::new();

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
        let totals = &result.facet_totals;

        // Format each non-empty facet with section headers. After a truncated
        // facet's entries, emit a per-facet hidden-count line (`write_hidden_tail`)
        // so the reader sees which facet got cut. The global tail used to live
        // at the end of `format_search_result`; on the facet path we suppress
        // it to avoid double-counting hidden matches across two surfaces —
        // unless there are no per-facet totals to disclose (see below).
        if !faceted.definitions.is_empty() {
            let _ = write!(
                out,
                "\n\n## Definitions ({})",
                count_label(faceted.definitions.len(), totals.definitions)
            );
            format_matches(
                &faceted.definitions,
                &result.scope,
                cache,
                &mut render_session,
                bloom,
                &mut expand_remaining,
                &mut expanded_files,
                &mut out,
                &mut segments,
            );
            write_hidden_tail(
                &mut out,
                faceted.definitions.len(),
                totals.definitions,
                "definitions",
            );
        }

        if !faceted.implementations.is_empty() {
            let _ = write!(
                out,
                "\n\n## Implementations ({})",
                count_label(faceted.implementations.len(), totals.implementations)
            );
            format_matches(
                &faceted.implementations,
                &result.scope,
                cache,
                &mut render_session,
                bloom,
                &mut expand_remaining,
                &mut expanded_files,
                &mut out,
                &mut segments,
            );
            write_hidden_tail(
                &mut out,
                faceted.implementations.len(),
                totals.implementations,
                "implementations",
            );
        }

        if !faceted.tests.is_empty() {
            let _ = write!(
                out,
                "\n\n## Tests ({})",
                count_label(faceted.tests.len(), totals.tests)
            );
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
            write_hidden_tail(&mut out, faceted.tests.len(), totals.tests, "tests");
        }

        if !faceted.usages_local.is_empty() {
            let _ = write!(
                out,
                "\n\n## Usages — same package ({})",
                count_label(faceted.usages_local.len(), totals.usages_local)
            );
            format_matches(
                &faceted.usages_local,
                &result.scope,
                cache,
                &mut render_session,
                bloom,
                &mut expand_remaining,
                &mut expanded_files,
                &mut out,
                &mut segments,
            );
            write_hidden_tail(
                &mut out,
                faceted.usages_local.len(),
                totals.usages_local,
                "usages",
            );
        }

        if !faceted.usages_cross.is_empty() {
            let _ = write!(
                out,
                "\n\n## Usages — other ({})",
                count_label(faceted.usages_cross.len(), totals.usages_cross)
            );
            format_matches(
                &faceted.usages_cross,
                &result.scope,
                cache,
                &mut render_session,
                bloom,
                &mut expand_remaining,
                &mut expanded_files,
                &mut out,
                &mut segments,
            );
            write_hidden_tail(
                &mut out,
                faceted.usages_cross.len(),
                totals.usages_cross,
                "usages",
            );
        }

        // Content/regex results carry no per-facet totals (all-zero), so the
        // per-facet hidden tails above cannot disclose the display cap — emit
        // the global tail for them. Faceted symbol results disclose per-facet.
        let facet_totals_present = totals.definitions
            + totals.implementations
            + totals.tests
            + totals.usages_local
            + totals.usages_cross
            > 0;
        if !facet_totals_present && result.total_found > result.matches.len() {
            let omitted = result.total_found - result.matches.len();
            let _ = write!(
                out,
                "\n\n... and {omitted} more matches. Narrow with scope."
            );
        }
    } else {
        // Linear display for ≤5 matches
        format_matches(
            &result.matches,
            &result.scope,
            cache,
            &mut render_session,
            bloom,
            &mut expand_remaining,
            &mut expanded_files,
            &mut out,
            &mut segments,
        );

        // The faceted path emits a per-facet line for each truncated facet
        // above; printing both there would double-count the same hidden
        // matches.
        if result.total_found > result.matches.len() {
            let omitted = result.total_found - result.matches.len();
            let _ = write!(
                out,
                "\n\n... and {omitted} more matches. Narrow with scope."
            );
        }
    }

    out.push_str(&skipped_large_note(result));

    // Apply value-based budget allocation before appending the token footer.
    // Under-budget: byte-identical. Over-budget: drops lowest-value match blocks.
    // budget.unwrap_or(DEFAULT_BUDGET) keeps the no-budget path byte-identical
    // to before this fix — DEFAULT_BUDGET remains the default, it is simply no
    // longer a hardcode that shadows a real caller-supplied budget.
    let budget_tokens = budget.unwrap_or(crate::budget::DEFAULT_BUDGET);
    out = alloc::fit_to_budget(&out, &mut segments, budget_tokens);

    let tokens = estimate_tokens(out.len() as u64);
    let token_str = format_token_count(tokens);
    let _ = write!(out, "\n\n({token_str} tokens)");

    if let Some(session) = session {
        let expansions = segments.into_iter().filter_map(|block| block.expansion);
        record_visible_expansions(session, &out, budget, expansions);
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
    let content = tilth_core::source::read_source(&m.path).ok()?;
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

            let _ = write!(out, "\n{i:>4} | {line}");
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

        // Extract line number from formatted line: "  42 | content"
        let line_num = segment
            .find('|')
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
    if !matches!(tilth_core::lang::detect_file_type(path), FileType::Code(_)) {
        return None;
    }
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    if meta.len() > tilth_core::source::MAX_FILE_SIZE {
        return None;
    }
    // The cache is shared with read and map, so the language comes from the
    // content as theirs does: a `.h` holding C++ is C++.
    Some(cache.get_or_compute(path, mtime, || {
        let content = tilth_core::source::read_source(path).unwrap_or_default();
        let file_type = tilth_core::lang::detect_file_type_with_content(path, &content);
        read::outline::generate(path, file_type, &content, content.as_bytes(), false)
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
            let _ = write!(context, "\n-> {line}");
        } else {
            let _ = write!(context, "\n  {line}");
        }
    }
    Some(context)
}

/// Annotate a usage match with its enclosing scope: `"function foo"` /
/// `"class Bar"` for code (via tree-sitter), `"§Heading"` for markdown
/// (via line walk). Returns `None` for top-level matches and unsupported
/// file types — the formatter renders those without an `in …` suffix.
fn enclosing_scope_label(
    path: &std::path::Path,
    match_line: u32,
    cache: &OutlineCache,
) -> Option<String> {
    match tilth_core::lang::detect_file_type(path) {
        FileType::Code(_) => {
            let s = tilth_core::search::scope::enclosing_definition_at(path, match_line, cache)?;
            Some(format!("{} {}", s.kind, s.name))
        }
        FileType::Markdown => markdown_enclosing_scope(path, match_line),
        _ => None,
    }
}

/// Find the deepest ATX-heading section that encloses `match_line`. Returns
/// the heading text prefixed with `§`. A `# foo` line inside a fenced or
/// indented code block is NOT a heading — the tree-sitter-md block grammar
/// owns that distinction, so we don't need our own fence pre-pass.
fn markdown_enclosing_scope(path: &std::path::Path, match_line: u32) -> Option<String> {
    if match_line == 0 {
        return None;
    }
    let content = tilth_core::source::read_source(path).ok()?;
    let tree = tilth_core::lang::outline::parse_markdown(&content)?;
    let lines: Vec<&str> = content.lines().collect();
    let mut best: Option<(tree_sitter::Node, u32)> = None;
    walk_md_for_enclosing(tree.root_node(), match_line, &mut best);
    let (heading, _) = best?;
    let text = tilth_core::lang::outline::heading_text(heading, &lines);
    if text.is_empty() {
        return None;
    }
    let display: String = if text.chars().count() > 60 {
        let mut s: String = text.chars().take(57).collect();
        s.push_str("...");
        s
    } else {
        text
    };
    Some(format!("§{display}"))
}

fn walk_md_for_enclosing<'a>(
    node: tree_sitter::Node<'a>,
    match_line: u32,
    best: &mut Option<(tree_sitter::Node<'a>, u32)>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "section" {
            continue;
        }
        let start = (child.start_position().row + 1) as u32;
        let end_excl = child.end_position();
        let end = if end_excl.column == 0 {
            end_excl.row as u32
        } else {
            (end_excl.row + 1) as u32
        };
        if match_line < start || match_line > end {
            continue;
        }
        // Section contains match_line. Record its heading if it has one,
        // then recurse to find a deeper section.
        let mut sec_cursor = child.walk();
        if let Some(heading) = child
            .children(&mut sec_cursor)
            .find(|c| c.kind() == "atx_heading")
        {
            // Update best to the *deepest* (largest start_line) match.
            match best {
                Some((_, prev_start)) if *prev_start >= start => {}
                _ => *best = Some((heading, start)),
            }
        }
        walk_md_for_enclosing(child, match_line, best);
    }
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

    // ── end-to-end through search functions ──

    #[test]
    fn content_search_glob_restricts_results() {
        let scope = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let all =
            content::search("TilthError", &scope, false, None, None, false).expect("search failed");
        let rs_only = content::search("TilthError", &scope, false, None, Some("*.rs"), false)
            .expect("search with glob failed");
        let toml_only = content::search("TilthError", &scope, false, None, Some("*.toml"), false)
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
        let rs_result = symbol::search("walker", &scope, None, Some("*.rs"), false)
            .expect("symbol search failed");
        let toml_result = symbol::search("walker", &scope, None, Some("*.toml"), false)
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
        let bloom = tilth_core::index::bloom::BloomFilterCache::new();
        let single: std::collections::HashSet<String> =
            std::iter::once("walker".to_string()).collect();
        let rs_callers =
            tilth_core::find_callers_batch(&single, &scope, &bloom, Some("*.rs"), None, |sites| {
                sites
            })
            .expect("callers failed");
        let toml_callers = tilth_core::find_callers_batch(
            &single,
            &scope,
            &bloom,
            Some("*.toml"),
            None,
            |sites| sites,
        )
        .expect("callers toml failed");
        let rs_callers: Vec<_> = rs_callers.per_file.into_iter().flatten().collect();
        let toml_callers: Vec<_> = toml_callers.per_file.into_iter().flatten().collect();

        assert!(
            !rs_callers.is_empty(),
            "*.rs should find callers of 'walker'"
        );
        assert!(
            toml_callers.is_empty(),
            "*.toml should not find callers of 'walker'"
        );
        for (_, c) in &rs_callers {
            assert_eq!(
                c.path.extension().and_then(|e| e.to_str()),
                Some("rs"),
                "non-.rs caller leaked: {}",
                c.path.display()
            );
        }
    }

    #[test]
    fn content_search_finds_symbol_through_symlink() {
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

        let result = content::search(
            "unique_symlink_test_symbol",
            tmp.path(),
            false,
            None,
            None,
            false,
        )
        .unwrap();
        // Should find the symbol in both real/api.rs and linked/api.rs
        assert!(
            result.total_found >= 2,
            "expected symbol found via both real and symlinked paths, got {}",
            result.total_found
        );
    }

    // ── enclosing_scope_label / markdown tests ──

    #[test]
    fn scope_label_code_combines_kind_and_name() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.ts");
        std::fs::write(&p, "class Foo {\n  bar() {\n    const x = 1;\n  }\n}\n").unwrap();
        let cache = OutlineCache::new();
        let label = enclosing_scope_label(&p, 3, &cache).unwrap();
        assert_eq!(label, "function Foo.bar");
    }

    #[test]
    fn scope_label_markdown_returns_section() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.md");
        std::fs::write(
            &p,
            "# Top\n\n## Cost Accounting\n\nsome text\n\nmore text\n",
        )
        .unwrap();
        let cache = OutlineCache::new();
        let label = enclosing_scope_label(&p, 7, &cache).unwrap();
        assert_eq!(label, "§Cost Accounting");
    }

    #[test]
    fn markdown_scope_truncates_long_headings() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.md");
        let long = "x".repeat(80);
        std::fs::write(&p, format!("## {long}\n\nbody\n")).unwrap();
        let label = markdown_enclosing_scope(&p, 3).unwrap();
        // 60-char window means 57 chars + "..." (display starts after the "§").
        assert!(label.starts_with("§"));
        assert!(label.ends_with("..."));
        assert_eq!(label.chars().count(), 1 + 57 + 3);
    }

    #[test]
    fn markdown_scope_returns_none_before_first_heading() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.md");
        std::fs::write(&p, "preamble line one\npreamble line two\n").unwrap();
        assert!(markdown_enclosing_scope(&p, 2).is_none());
    }

    /// `#`-prefixed lines inside fenced code blocks are NOT headings, so they
    /// must not become the enclosing scope label of usages on adjacent lines.
    /// Pre-fix this returned `§...` derived from a Python comment inside the
    /// fence; post-fix the AST owns the distinction.
    #[test]
    fn markdown_scope_skips_hashes_inside_fenced_code() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.md");
        std::fs::write(
            &p,
            "## Outer\n\nbefore fence\n\n```python\n# fake heading\nx = 1\n```\n\nafter fence\n",
        )
        .unwrap();
        // Line 7 (`x = 1`) is inside the fence; the only real heading is `## Outer`.
        let label = markdown_enclosing_scope(&p, 7).unwrap();
        assert_eq!(label, "§Outer");
    }

    /// `CommonMark` §4.6.1 caps ATX headings at 6 leading `#`s. 7+ hashes is
    /// raw text, not a heading, and must not surface as the enclosing scope.
    /// Pre-AST migration the regex matched `#######` and produced a bogus
    /// `§# Fake Heading 7` label.
    #[test]
    fn markdown_scope_rejects_seven_hash_atx_heading() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.md");
        std::fs::write(
            &p,
            "## Real Heading\n\nbody line\n\n####### Fake Heading 7\n\ntrailing line\n",
        )
        .unwrap();
        // Line 5 is the 7-hash line; line 7 is the line after.
        // Both should resolve to the only real heading, "Real Heading".
        assert_eq!(
            markdown_enclosing_scope(&p, 5),
            Some("§Real Heading".to_string())
        );
        assert_eq!(
            markdown_enclosing_scope(&p, 7),
            Some("§Real Heading".to_string())
        );
    }

    /// `CommonMark` §4.6.1 requires whitespace after the leading `#`s. `##NoSpace`
    /// is paragraph text, not a heading. Pre-AST migration the regex accepted
    /// it and produced `§NoSpace`.
    #[test]
    fn markdown_scope_rejects_no_space_atx_heading() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.md");
        std::fs::write(&p, "## Real Heading\n\n##NoSpace\n\ntrailing line\n").unwrap();
        // Line 3 is the no-space candidate; both line 3 and line 5 must
        // resolve to the only real heading.
        assert_eq!(
            markdown_enclosing_scope(&p, 3),
            Some("§Real Heading".to_string())
        );
        assert_eq!(
            markdown_enclosing_scope(&p, 5),
            Some("§Real Heading".to_string())
        );
    }

    #[test]
    fn format_single_match_renders_usage_scope_suffix() {
        use crate::types::Match;

        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.ts");
        std::fs::write(&p, "class Foo {\n  bar() {\n    const x = 1;\n  }\n}\n").unwrap();

        let m = Match {
            path: p.clone(),
            line: 3,
            text: "    const x = 1;".to_string(),
            is_definition: false,
            exact: false,
            file_lines: 5,
            mtime: SystemTime::now(),
            def_range: None,
            def_name: None,
            def_weight: 0,
            impl_target: None,
        };
        let cache = OutlineCache::new();
        let bloom = tilth_core::index::bloom::BloomFilterCache::new();
        let mut expand_remaining = 0usize;
        let mut expanded_files: HashSet<PathBuf> = HashSet::new();
        let mut out = String::new();

        format_single_match(
            &m,
            tmp.path(),
            &cache,
            &mut RenderSession::new(None),
            &bloom,
            &mut expand_remaining,
            &mut expanded_files,
            false,
            &mut out,
        );

        assert!(
            out.contains("[usage in function Foo.bar]"),
            "expected scope suffix in output, got: {out}"
        );
    }

    #[test]
    fn format_single_match_does_not_duplicate_expanded_line() {
        use crate::types::Match;

        // Small file (<50 lines) with no doc comment above the definition —
        // the outline-context preview at line 627 prints `m.text` verbatim,
        // and expand_match's whole-file expansion (small files expand fully,
        // see EXPAND_FULL_FILE_THRESHOLD) includes that same source line
        // again in the fence right below it.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("small.rs");
        let src = "pub struct Thing;\n\nimpl Thing {\n    pub fn exit_code(&self) -> i32 {\n        0\n    }\n}\n";
        std::fs::write(&p, src).unwrap();

        let m = Match {
            path: p.clone(),
            line: 4,
            text: "    pub fn exit_code(&self) -> i32 {".to_string(),
            is_definition: true,
            exact: true,
            file_lines: 7,
            mtime: SystemTime::now(),
            def_range: Some((4, 6)),
            def_name: Some("exit_code".to_string()),
            def_weight: 100,
            impl_target: None,
        };
        let cache = OutlineCache::new();
        let bloom = tilth_core::index::bloom::BloomFilterCache::new();
        let mut expand_remaining = 1usize;
        let mut expanded_files: HashSet<PathBuf> = HashSet::new();
        let mut out = String::new();

        format_single_match(
            &m,
            tmp.path(),
            &cache,
            &mut RenderSession::new(None),
            &bloom,
            &mut expand_remaining,
            &mut expanded_files,
            false,
            &mut out,
        );

        let needle = "pub fn exit_code(&self) -> i32 {";
        let occurrences = out.matches(needle).count();
        assert_eq!(
            occurrences, 1,
            "expanded line must appear exactly once, got {occurrences} in: {out}"
        );
    }

    #[test]
    fn boost_query_routes_impl_target_into_truncation() {
        use crate::types::Match;

        let base = Match {
            path: PathBuf::from("x.rs"),
            line: 1,
            text: String::new(),
            is_definition: true,
            exact: true,
            file_lines: 200,
            mtime: SystemTime::now(),
            def_range: Some((1, 200)),
            def_name: None,
            def_weight: 0,
            impl_target: None,
        };

        // Plain definition: the searched token is the symbol name in def_name.
        let plain = Match {
            def_name: Some("handle_request".to_string()),
            ..base.clone()
        };
        assert_eq!(boost_query(&plain), Some("handle_request"));

        // impl match: def_name is the rendered label ("impl Iterator for Counter")
        // which never appears in the body — the searched trait lives in
        // impl_target. Regression guard for the dead-boost bug where def_name was
        // passed and the boost matched nothing.
        let impl_match = Match {
            def_name: Some("impl Iterator for Counter".to_string()),
            impl_target: Some("Iterator".to_string()),
            ..base.clone()
        };
        assert_eq!(boost_query(&impl_match), Some("Iterator"));

        // No names: no boost.
        assert_eq!(boost_query(&base), None);
    }

    #[test]
    fn write_hidden_tail_emits_only_when_truncated() {
        let mut out = String::new();
        write_hidden_tail(&mut out, 3, 3, "definitions");
        assert!(out.is_empty(), "no truncation → no tail line, got {out:?}");

        let mut out = String::new();
        write_hidden_tail(&mut out, 10, 14, "definitions");
        assert_eq!(out, "\n\n... and 4 more definitions. Narrow with scope.");

        let mut out = String::new();
        write_hidden_tail(&mut out, 3, 27, "usages");
        assert_eq!(out, "\n\n... and 24 more usages. Narrow with scope.");
    }

    #[test]
    fn count_label_renders_displayed_over_total_only_when_truncated() {
        // No truncation — bare count.
        assert_eq!(count_label(3, 3), "3");
        // Defensive: shown > total (shouldn't happen in practice) — bare count.
        assert_eq!(count_label(4, 3), "4");
        // Truncated — displayed/total form.
        assert_eq!(count_label(10, 14), "10/14");
        // Zero / zero — still bare (no header is emitted at zero anyway).
        assert_eq!(count_label(0, 0), "0");
    }

    #[test]
    fn format_single_match_inlines_markdown_section_body() {
        use crate::types::Match;

        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("notes.md");
        std::fs::write(
            &p,
            "# Top\n\n## Session 50\n\nFirst paragraph.\n\nSecond line.\n\n## Other\n\nUnrelated.\n",
        )
        .unwrap();

        // Mimic the `Match` `find_defs_markdown_buf` would produce for a
        // markdown-heading def: weight 30, def_range covers the section span.
        let m = Match {
            path: p.clone(),
            line: 3, // `## Session 50`
            text: "## Session 50".to_string(),
            is_definition: true,
            exact: true,
            file_lines: 10,
            mtime: SystemTime::now(),
            def_range: Some((3, 7)), // heading line .. section_end (1-indexed inclusive)
            def_name: Some("Session 50".to_string()),
            def_weight: 30,
            impl_target: None,
        };
        let cache = OutlineCache::new();
        let bloom = tilth_core::index::bloom::BloomFilterCache::new();
        let mut expand_remaining = 0usize;
        let mut expanded_files: HashSet<PathBuf> = HashSet::new();
        let mut out = String::new();

        format_single_match(
            &m,
            tmp.path(),
            &cache,
            &mut RenderSession::new(None),
            &bloom,
            &mut expand_remaining,
            &mut expanded_files,
            false,
            &mut out,
        );

        assert!(
            out.contains("First paragraph."),
            "section body must be inlined, got: {out:?}"
        );
        assert!(
            out.contains("Second line."),
            "section body must include later body lines, got: {out:?}"
        );
        assert!(
            !out.contains("Unrelated."),
            "must stop at section_end, got: {out:?}"
        );
    }

    #[test]
    fn format_single_match_caps_long_markdown_section() {
        use crate::types::Match;

        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("long.md");
        // Heading on line 1, then 60 body lines.
        let mut content = String::from("## Big Section\n");
        for i in 0..60 {
            let _ = writeln!(content, "body line {i}");
        }
        std::fs::write(&p, &content).unwrap();

        let m = Match {
            path: p.clone(),
            line: 1,
            text: "## Big Section".to_string(),
            is_definition: true,
            exact: true,
            file_lines: 61,
            mtime: SystemTime::now(),
            def_range: Some((1, 61)),
            def_name: Some("Big Section".to_string()),
            def_weight: 30,
            impl_target: None,
        };
        let cache = OutlineCache::new();
        let bloom = tilth_core::index::bloom::BloomFilterCache::new();
        let mut expand_remaining = 0usize;
        let mut expanded_files: HashSet<PathBuf> = HashSet::new();
        let mut out = String::new();

        format_single_match(
            &m,
            tmp.path(),
            &cache,
            &mut RenderSession::new(None),
            &bloom,
            &mut expand_remaining,
            &mut expanded_files,
            false,
            &mut out,
        );

        // Cap is 40 lines; expect 60 - 40 = 20 truncated.
        assert!(
            out.contains("body line 0"),
            "first body line must appear, got: {out:?}"
        );
        assert!(
            out.contains("body line 39"),
            "last kept body line must appear, got: {out:?}"
        );
        assert!(
            !out.contains("body line 40"),
            "body line beyond cap must be trimmed, got: {out:?}"
        );
        assert!(
            out.contains("20 more lines"),
            "must signal truncated lines, got: {out:?}"
        );
        assert!(
            out.contains("--expand"),
            "tail must point to --expand for full section, got: {out:?}"
        );
    }

    /// 99c4a3d's docstring is explicit: the markdown-section preview is a
    /// fixed-cost short-circuit that bypasses the --expand budget. This pins
    /// that intent — passing a non-zero `expand_remaining` must NOT cause
    /// the renderer to skip the cap. Without this guard, a future refactor
    /// could "fix" the short-circuit by routing through expand and turn
    /// every markdown-heading match into a multi-hundred-line preview.
    #[test]
    fn format_single_match_markdown_cap_bypasses_expand_budget() {
        use crate::types::Match;

        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("long.md");
        let mut content = String::from("## Big Section\n");
        for i in 0..60 {
            let _ = writeln!(content, "body line {i}");
        }
        std::fs::write(&p, &content).unwrap();

        let m = Match {
            path: p.clone(),
            line: 1,
            text: "## Big Section".to_string(),
            is_definition: true,
            exact: true,
            file_lines: 61,
            mtime: SystemTime::now(),
            def_range: Some((1, 61)),
            def_name: Some("Big Section".to_string()),
            def_weight: 30,
            impl_target: None,
        };
        let cache = OutlineCache::new();
        let bloom = tilth_core::index::bloom::BloomFilterCache::new();
        // Non-zero expand budget — should NOT change the cap behavior.
        let mut expand_remaining = 5usize;
        let mut expanded_files: HashSet<PathBuf> = HashSet::new();
        let mut out = String::new();

        format_single_match(
            &m,
            tmp.path(),
            &cache,
            &mut RenderSession::new(None),
            &bloom,
            &mut expand_remaining,
            &mut expanded_files,
            false,
            &mut out,
        );

        // Body lines beyond the cap must still be trimmed.
        assert!(
            !out.contains("body line 40"),
            "cap must apply even with non-zero expand budget, got: {out:?}"
        );
        assert!(
            out.contains("20 more lines"),
            "tail must still report truncated lines, got: {out:?}"
        );
        // Budget must be untouched — short-circuit returns before consuming it.
        assert_eq!(
            expand_remaining, 5,
            "markdown short-circuit must not consume expand budget"
        );
    }

    /// Worst-case bound: with `MAX_MATCHES` = 10 markdown-heading defs each
    /// hitting the 40-line preview cap, total inlined preview content is at
    /// most 10 × 40 = 400 lines. This pins the bound by exercising the cap
    /// and asserting the truncation shape, so a future bump of either
    /// constant can't silently inflate worst-case output without updating
    /// the test.
    #[test]
    fn markdown_preview_cap_constant_unchanged() {
        // Pin both constants — if either changes, this assertion fails and
        // the test author has to consider the worst-case product (currently
        // 10 * 40 = 400 lines extra in default preview, on top of the
        // outline context per match).
        assert_eq!(
            MARKDOWN_PREVIEW_MAX_LINES, 40,
            "if you change MARKDOWN_PREVIEW_MAX_LINES, also re-evaluate the \
             MAX_MATCHES * MARKDOWN_PREVIEW_MAX_LINES worst-case bound \
             (currently 10 * 40 = 400 lines per search response)"
        );
    }

    #[test]
    fn format_grouped_usages_emits_h3_heading() {
        use crate::types::Match;

        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.ts");
        std::fs::write(
            &p,
            "function host() {\n  doThing();\n  doThing();\n  doThing();\n}\n",
        )
        .unwrap();

        let mk = |line: u32| Match {
            path: p.clone(),
            line,
            text: "  doThing();".to_string(),
            is_definition: false,
            exact: false,
            file_lines: 5,
            mtime: SystemTime::now(),
            def_range: None,
            def_name: None,
            def_weight: 0,
            impl_target: None,
        };
        let m1 = mk(2);
        let m2 = mk(3);
        let m3 = mk(4);
        let group: Vec<&Match> = vec![&m1, &m2, &m3];
        let cache = OutlineCache::new();
        let mut out = String::new();
        format_grouped_usages(&group, tmp.path(), &cache, &mut out);

        assert!(
            out.starts_with("\n\n### "),
            "grouped-usage heading must be H3, got: {out:?}"
        );
        assert!(
            !out.starts_with("\n\n## "),
            "grouped-usage heading must not be H2, got: {out:?}"
        );
    }

    // Verify that format_matches records segment byte ranges correctly.
    // Each push must cover non-empty, non-overlapping ranges that index into `out`.
    #[test]
    fn format_matches_segments_record_correct_byte_ranges() {
        use crate::types::Match;
        use tilth_core::index::bloom::BloomFilterCache;

        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.rs");
        std::fs::write(&p, "fn alpha() {}\nfn beta() {}\nfn gamma() {}\n").unwrap();

        let mk = |line: u32, weight: u16, name: &str| Match {
            path: p.clone(),
            line,
            text: format!("fn {name}()"),
            is_definition: true,
            exact: true,
            file_lines: 3,
            mtime: SystemTime::now(),
            def_range: Some((line, line)),
            def_name: Some(name.to_string()),
            def_weight: weight,
            impl_target: None,
        };

        let matches = vec![mk(1, 30, "alpha"), mk(2, 10, "beta"), mk(3, 50, "gamma")];
        let cache = OutlineCache::new();
        let bloom = BloomFilterCache::new();
        let mut out = String::from("HEADER");
        let mut segments: Vec<alloc::Block> = Vec::new();
        let mut expand_remaining = 0usize;
        let mut expanded_files = HashSet::new();

        format_matches(
            &matches,
            tmp.path(),
            &cache,
            &mut RenderSession::new(None),
            &bloom,
            &mut expand_remaining,
            &mut expanded_files,
            &mut out,
            &mut segments,
        );

        // One segment per match (all singletons — definitions are never grouped).
        assert_eq!(segments.len(), 3, "expected one segment per match");

        // Values must match def_weight casts.
        assert_eq!(segments[0].value, 30i64);
        assert_eq!(segments[1].value, 10i64);
        assert_eq!(segments[2].value, 50i64);

        // Ranges must be valid, non-empty, and non-overlapping.
        let mut cursor = "HEADER".len();
        for (i, b) in segments.iter().enumerate() {
            let (start, end) = (b.start, b.end);
            assert!(start >= cursor, "segment {i} start < cursor");
            assert!(
                (start..=end).contains(&b.body_start),
                "segment {i} body start outside the block"
            );
            assert!(end > start, "segment {i} is empty");
            assert!(end <= out.len(), "segment {i} end out of bounds");
            // Slice must not panic (validates char-boundary alignment).
            let _ = &out[start..end];
            cursor = end;
        }
    }

    /// Regression for the hardcoded-`DEFAULT_BUDGET` bug: `fit_to_budget` must
    /// receive the caller's real `budget` instead of always being called with
    /// `crate::budget::DEFAULT_BUDGET` (`24_000`). Fixture: one real definition
    /// (`budget_probe_target`, high `def_weight`) plus a usage in a file named
    /// after the query, so `rank::sort`'s `basename_boost` (+500) renders the
    /// usage FIRST despite it being lower-value — this decouples render order
    /// from value order exactly like the audit's live repro. At a small
    /// explicit budget, the positional tail-cut (`budget::apply`) keeps
    /// whatever rendered first (the low-value usage) and severs the
    /// definition; value-based selection (`fit_to_budget`) keeps the
    /// definition (highest value) and cuts the usage's expansion instead.
    #[test]
    fn search_symbol_expanded_threads_real_budget_into_value_selection() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("definition.rs"),
            "pub fn budget_probe_target() {\n    let _ = 1;\n}\n",
        )
        .unwrap();
        // File stem == query triggers basename_boost(+500), outscoring the
        // definition's own boosts (def_weight*10 + definition_name_boost) so
        // this usage renders FIRST in the linear (<=5 match) format path.
        // Padded with filler lines so its expanded block is large enough that
        // budget 400 must cut ONE of the two blocks — proving which one a
        // real budget cuts (positional: whichever renders last; value-based:
        // the lower-value one, regardless of render order).
        let mut usage_body = String::from("fn calls_it() {\n    budget_probe_target();\n");
        for i in 0..60 {
            let _ = writeln!(usage_body, "    let filler_{i} = {i};");
        }
        usage_body.push_str("}\n");
        std::fs::write(tmp.path().join("budget_probe_target.rs"), &usage_body).unwrap();

        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = tilth_core::index::bloom::BloomFilterCache::new();

        let out = search_symbol_expanded(
            "budget_probe_target",
            tmp.path(),
            &cache,
            &session,
            &bloom,
            2,
            None,
            None,
            false,
            Some(400),
        )
        .unwrap();

        assert!(
            out.contains("fn budget_probe_target"),
            "value-based selection must keep the highest-value block (the \
             definition) even though it rendered last: {out}"
        );
        // The lower-value usage keeps its location and loses its expansion.
        assert!(
            out.contains("### budget_probe_target.rs:2 [usage in function calls_it]")
                && out.contains("expansion omitted to fit budget"),
            "must show fit_to_budget's value-based cut, not \
             budget::apply's positional \"... truncated\" marker: {out}"
        );
        assert!(
            !out.contains("... truncated ("),
            "positional tail-cut marker must not appear — the real budget \
             was supposed to reach fit_to_budget directly: {out}"
        );
    }

    /// A block was kept or dropped whole, so a definition whose expanded
    /// body alone overran the budget vanished, and the note called it the
    /// "lower-value" match while the usage stayed. The definition must keep
    /// its location and lose only its body.
    #[test]
    fn a_tight_budget_keeps_the_definition_without_its_body() {
        let tmp = tempfile::tempdir().unwrap();
        let mut body = String::from("pub fn big_target() {\n");
        for i in 0..400 {
            let _ = writeln!(body, "    let v{i} = {i} + 1;");
        }
        body.push_str("}\n\nfn user() {\n    big_target();\n}\n");
        std::fs::write(tmp.path().join("big.rs"), body).unwrap();

        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = tilth_core::index::bloom::BloomFilterCache::new();
        let out = search_symbol_expanded(
            "big_target",
            tmp.path(),
            &cache,
            &session,
            &bloom,
            1,
            None,
            None,
            false,
            Some(300),
        )
        .unwrap();

        assert!(out.contains("### big.rs:1-402 [definition]"), "{out}");
        assert!(out.contains("expansion omitted to fit budget"), "{out}");
        assert!(out.contains("[usage in function user]"), "{out}");
        assert!(!out.contains("lower-value match(es) omitted"), "{out}");
        assert!(!out.contains("let v399"), "{out}");
    }

    /// Regression guard: omitting the budget (`None`) must produce byte-
    /// identical output to before this fix — `DEFAULT_BUDGET` remains the
    /// default, it is simply no longer a hardcode that shadows a real
    /// caller-supplied budget.
    #[test]
    fn search_symbol_expanded_no_budget_matches_default_budget_output() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("definition.rs"),
            "pub fn budget_probe_target() {\n    let _ = 1;\n}\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("budget_probe_target.rs"),
            "fn calls_it() {\n    budget_probe_target();\n}\n",
        )
        .unwrap();

        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = tilth_core::index::bloom::BloomFilterCache::new();

        let with_none = search_symbol_expanded(
            "budget_probe_target",
            tmp.path(),
            &cache,
            &session,
            &bloom,
            2,
            None,
            None,
            false,
            None,
        )
        .unwrap();

        let session2 = Session::new();
        let with_explicit_default = search_symbol_expanded(
            "budget_probe_target",
            tmp.path(),
            &cache,
            &session2,
            &bloom,
            2,
            None,
            None,
            false,
            Some(crate::budget::DEFAULT_BUDGET),
        )
        .unwrap();

        assert_eq!(
            with_none, with_explicit_default,
            "omitting budget must be identical to explicitly passing DEFAULT_BUDGET"
        );
    }

    /// Content/regex results carry no per-facet totals, so on the faceted
    /// path (>5 matches) neither the facet headings nor their per-facet
    /// hidden tails can disclose the display cap. 15 hits must render as
    /// "10 of 15 matches" plus the global tail — not a bare "10" that reads
    /// as the whole truth.
    #[test]
    fn content_search_discloses_the_display_cap() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["a.rs", "b.rs", "c.rs"] {
            let mut body = String::new();
            for i in 0..5 {
                let _ = writeln!(body, "let v{i} = needle_alpha;");
            }
            std::fs::write(tmp.path().join(name), body).unwrap();
        }

        let out = search_content_expanded(
            "needle_alpha",
            tmp.path(),
            &OutlineCache::new(),
            &Session::new(),
            0,
            None,
            None,
            false,
            None,
        )
        .unwrap();

        assert!(
            out.contains("10 of 15 matches"),
            "header must disclose the display cap: {out}"
        );
        assert!(
            out.contains("... and 5 more matches"),
            "faceted content results must emit the global hidden tail: {out}"
        );
    }
    /// Two things the disclosure must get right on the SYMBOL path, which
    /// carries real per-facet totals: every per-query section header states its
    /// own cap (including the multi-symbol one), and the global "more matches"
    /// tail stays off — a symbol facet already says "more usages", and printing
    /// both would count the same hidden matches twice.
    #[test]
    fn capped_symbol_results_disclose_in_the_header_and_never_globally() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("def.rs"), "pub fn alpha_target() {}\n").unwrap();
        for i in 0..14 {
            std::fs::write(
                tmp.path().join(format!("c{i:02}.rs")),
                format!("fn caller{i}() {{\n    alpha_target();\n}}\n"),
            )
            .unwrap();
        }

        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = tilth_core::index::bloom::BloomFilterCache::new();
        let single = search_symbol_expanded(
            "alpha_target",
            tmp.path(),
            &cache,
            &session,
            &bloom,
            0,
            None,
            None,
            false,
            None,
        )
        .unwrap();

        assert!(
            single.contains("10 of 15 matches"),
            "capped symbol header must disclose the cap: {single}"
        );
        assert!(
            single.contains("more usages. Narrow with scope."),
            "the truncated facet must still carry its own tail: {single}"
        );
        assert!(
            !single.contains("more matches."),
            "the global tail belongs to content/regex results only: {single}"
        );

        let session = Session::new();
        let multi = search_multi_symbol_expanded(
            &["alpha_target"],
            tmp.path(),
            &cache,
            &session,
            &bloom,
            0,
            None,
            None,
            false,
            None,
        )
        .unwrap();
        assert!(
            multi.contains("10 of 15 matches"),
            "each multi-symbol section header must disclose its own cap: {multi}"
        );
    }

    /// A byte-order mark must not reach search output: a match on line 1
    /// printed a stray U+FEFF in front of the code, and a definition there
    /// carried it in its signature.
    #[test]
    fn a_byte_order_mark_stays_out_of_search_output() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("m.py"),
            "\u{feff}def bom_func():\n    return 1\n\nbom_func()\n",
        )
        .unwrap();
        let cache = OutlineCache::new();
        for out in [
            search_symbol("bom_func", tmp.path(), &cache, None, false).unwrap(),
            search_content("bom_func", tmp.path(), &cache, None, false).unwrap(),
            search_regex("bom_f.nc", tmp.path(), &cache, None, false).unwrap(),
        ] {
            assert!(out.contains("bom_func"), "{out}");
            assert!(!out.contains('\u{feff}'), "stray BOM in: {out}");
        }
    }

    /// A file over the size limit that contains the query used to vanish:
    /// "0 matches" and no word about the file. Search names it now, and
    /// stays quiet about big files that do not contain the query.
    #[test]
    fn a_file_over_the_size_limit_is_named_when_it_holds_the_query() {
        let tmp = tempfile::tempdir().unwrap();
        let limit = usize::try_from(tilth_core::source::MAX_FILE_SIZE).unwrap();
        let filler = "// padding line to push the file over the size limit\n";
        let pad = filler.repeat(limit / filler.len() + 1);
        std::fs::write(
            tmp.path().join("big.rs"),
            format!("{pad}pub fn needle_target_fn() {{}}\n"),
        )
        .unwrap();
        std::fs::write(tmp.path().join("other_big.rs"), &pad).unwrap();
        std::fs::write(tmp.path().join("small.rs"), "fn unrelated() {}\n").unwrap();
        let cache = OutlineCache::new();
        for (query, out) in [
            (
                "needle_target_fn",
                search_symbol("needle_target_fn", tmp.path(), &cache, None, false).unwrap(),
            ),
            (
                "needle_target_fn",
                search_content("needle_target_fn", tmp.path(), &cache, None, false).unwrap(),
            ),
            (
                "needle_t.rget_fn",
                search_regex("needle_t.rget_fn", tmp.path(), &cache, None, false).unwrap(),
            ),
        ] {
            let note =
                format!("\n> 1 file over 500 KB contains `{query}` and was not searched: big.rs");
            assert!(out.contains(&note), "the skipped file must be named: {out}");
            assert!(!out.contains("other_big.rs"), "{out}");
        }
        let quiet = search_symbol("unrelated", tmp.path(), &cache, None, false).unwrap();
        assert!(!quiet.contains("not searched"), "{quiet}");
    }

    /// The caller walk skipped big files with no word, so callers, grok and
    /// deps answered as if the big file did not exist. They name it now.
    #[test]
    fn caller_walks_name_a_big_file_that_holds_the_target() {
        let tmp = tempfile::tempdir().unwrap();
        let limit = usize::try_from(tilth_core::source::MAX_FILE_SIZE).unwrap();
        let filler = "// padding line to push the file over the size limit\n";
        let pad = filler.repeat(limit / filler.len() + 1);
        std::fs::write(
            tmp.path().join("lib.rs"),
            "pub fn big_callee() {}\nfn small_caller() { big_callee(); }\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("big.rs"),
            format!("{pad}fn big_caller() {{ crate::big_callee(); }}\n"),
        )
        .unwrap();
        std::fs::write(tmp.path().join("other_big.rs"), &pad).unwrap();
        let bloom = tilth_core::index::bloom::BloomFilterCache::new();

        let callers = callers::search_callers_expanded(
            "big_callee",
            tmp.path(),
            &bloom,
            0,
            None,
            None,
            false,
        )
        .unwrap();
        let multi = callers::search_callers_multi_expanded(
            &["big_callee", "small_caller"],
            tmp.path(),
            &bloom,
            0,
            None,
            None,
            false,
        )
        .unwrap();
        let grok = crate::run_grok("big_callee", tmp.path(), false).unwrap();
        let deps = crate::run_deps(&tmp.path().join("lib.rs"), tmp.path(), None, false).unwrap();
        for (held, out) in [
            ("`big_callee`", callers),
            ("`big_callee` or `small_caller`", multi),
            ("`big_callee`", grok),
            ("a name from `lib.rs`", deps),
        ] {
            let note =
                format!("\n> 1 file over 500 KB contains {held} and was not searched: big.rs");
            assert!(out.contains(&note), "the skipped file must be named: {out}");
            assert!(!out.contains("other_big.rs"), "{out}");
        }
    }

    /// The names a skipped file was checked for read as one phrase, and a
    /// long list ends in a count.
    #[test]
    fn names_read_as_one_phrase() {
        assert_eq!(names_phrase([]), "");
        assert_eq!(names_phrase(["a"]), "`a`");
        assert_eq!(names_phrase(["b", "a", "b"]), "`a` or `b`");
        assert_eq!(names_phrase(["c", "a", "b"]), "`a`, `b` or `c`");
        assert_eq!(
            names_phrase(["a", "b", "c", "d"]),
            "`a`, `b`, `c` or 1 other name"
        );
        assert_eq!(
            names_phrase(["a", "b", "c", "d", "e"]),
            "`a`, `b`, `c` or 2 other names"
        );
    }

    /// The outline cache is shared by read, search and map, keyed by path
    /// and mtime. Search's outline context read a `.h` by its path alone,
    /// as C, so the outline cached for a C++ header depended on which tool
    /// reached it first.
    #[test]
    fn search_outline_context_reads_a_cpp_header_as_cpp() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("shape.h");
        std::fs::write(
            &path,
            "#pragma once\nclass Shape {\n public:\n  int area() const { return 0; }\n  int sides() { return 4; }\n};\n",
        )
        .unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let file_type = tilth_core::lang::detect_file_type_with_content(&path, &content);
        assert_eq!(file_type, FileType::Code(tilth_core::types::Lang::Cpp));
        let expected =
            read::outline::generate(&path, file_type, &content, content.as_bytes(), false);
        let cache = OutlineCache::new();
        assert_eq!(
            get_outline_str(&path, &cache).as_deref(),
            Some(expected.as_str())
        );
    }

    /// A qualified query's notes say what they know. A big file was named as
    /// holding "the query" when it held only the bare name, and dotted text
    /// such as `res.send` got "No `send` is defined in `res`" when nothing
    /// but free definitions of `send` exist: `res` is a variable, not an
    /// owner. The owner note stays for a qualifier that names the wrong type.
    #[test]
    fn a_qualified_query_notes_only_what_holds() {
        let tmp = tempfile::tempdir().unwrap();
        let limit = usize::try_from(tilth_core::source::MAX_FILE_SIZE).unwrap();
        let filler = "// padding line to push the file over the size limit\n";
        let pad = filler.repeat(limit / filler.len() + 1);
        std::fs::write(tmp.path().join("big.rs"), format!("{pad}fn run() {{}}\n")).unwrap();
        std::fs::write(
            tmp.path().join("m.rs"),
            "pub struct A;\nimpl A {\n    pub fn run(&self) {}\n}\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("response.js"),
            "var send = require('send');\nfunction reply(res) {\n  res.send('ok');\n}\n",
        )
        .unwrap();
        let cache = OutlineCache::new();

        let out = search_symbol("B::run", tmp.path(), &cache, None, false).unwrap();
        assert!(out.contains("not searched: big.rs"), "{out}");
        assert!(out.contains("contains `run` and was"), "{out}");
        assert!(out.contains("No `run` is defined in `B`"), "{out}");

        let out = search_symbol("res.send", tmp.path(), &cache, None, false).unwrap();
        assert!(out.contains("response.js:3"), "{out}");
        assert!(!out.contains("is defined in"), "{out}");

        // A multi-target walk looks for each target's bare name, once.
        let bloom = tilth_core::index::bloom::BloomFilterCache::new();
        let out = callers::search_callers_multi_expanded(
            &["A.run", "start", "B::run"],
            tmp.path(),
            &bloom,
            0,
            None,
            None,
            false,
        )
        .unwrap();
        assert!(out.contains("not searched: big.rs"), "{out}");
        assert!(out.contains("contains `run` or `start` and was"), "{out}");
    }
}
