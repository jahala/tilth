use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

use super::file_metadata;
use crate::lang::treesitter::{
    definition_weight, extract_definition_name, extract_impl_trait, extract_impl_type,
    extract_implemented_interfaces, DEFINITION_KINDS,
};

use crate::error::TilthError;
use crate::lang::detect_file_type;
use crate::lang::outline::outline_language;
use crate::search::rank;
use crate::types::{FileType, Match, SearchResult};
use grep_regex::RegexMatcher;
use grep_searcher::sinks::UTF8;
use grep_searcher::Searcher;

/// Multi-symbol batch search.
/// Single-walk: each file is opened/parsed once; AhoCorasick gates by any-query hit;
/// tree-sitter AST walked once with per-query buckets. Same for usages.
/// Returns one `SearchResult` per query in input order.
pub fn search_batch(
    queries: &[&str],
    scope: &Path,
    cache: Option<&crate::cache::OutlineCache>,
    context: Option<&Path>,
    glob: Option<&str>,
) -> Result<Vec<SearchResult>, TilthError> {
    if queries.is_empty() {
        return Ok(Vec::new());
    }
    if queries.len() == 1 {
        return Ok(vec![search(queries[0], scope, cache, context, glob)?]);
    }

    // Build aho-corasick automaton for byte-level any-of gate.
    let ac = aho_corasick::AhoCorasick::new(queries).map_err(|e| TilthError::InvalidQuery {
        query: queries.join(","),
        reason: e.to_string(),
    })?;

    // Build single regex \b(q1|q2|...)\b for usages.
    let alt = queries
        .iter()
        .map(|q| regex_syntax::escape(q))
        .collect::<Vec<_>>()
        .join("|");
    let pattern = format!(r"\b(?:{})\b", alt);
    let matcher = RegexMatcher::new(&pattern).map_err(|e| TilthError::InvalidQuery {
        query: queries.join(","),
        reason: e.to_string(),
    })?;

    let (defs_by_q, usages_by_q) = rayon::join(
        || find_definitions_batch(queries, &ac, scope, glob, cache),
        || find_usages_batch(queries, &matcher, scope, glob),
    );

    let defs_by_q = defs_by_q?;
    let usages_by_q = usages_by_q?;

    let mut out = Vec::with_capacity(queries.len());
    for (i, query) in queries.iter().enumerate() {
        let defs = defs_by_q[i].clone();
        let usages = usages_by_q[i].clone();
        let mut merged: Vec<Match> = defs;
        let def_count = merged.len();
        for m in usages {
            let dominated = merged[..def_count]
                .iter()
                .any(|d| d.path == m.path && d.line == m.line);
            if !dominated {
                merged.push(m);
            }
        }
        let total = merged.len();
        let usage_count = total - def_count;
        rank::sort(&mut merged, query, scope, context);
        out.push(SearchResult {
            query: (*query).to_string(),
            scope: scope.to_path_buf(),
            matches: merged,
            total_found: total,
            definitions: def_count,
            usages: usage_count,
            has_more: false,
            offset: 0,
        });
    }
    Ok(out)
}

fn find_definitions_batch(
    queries: &[&str],
    ac: &aho_corasick::AhoCorasick,
    scope: &Path,
    glob: Option<&str>,
    cache: Option<&crate::cache::OutlineCache>,
) -> Result<Vec<Vec<Match>>, TilthError> {
    let buckets: Mutex<Vec<Vec<Match>>> = Mutex::new(vec![Vec::new(); queries.len()]);
    let walker = super::walker(scope, glob)?;

    walker.run(|| {
        let buckets = &buckets;
        let cache = cache;
        Box::new(move |entry| {
            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }
            let path = entry.path();
            let file_size = match std::fs::metadata(path) {
                Ok(meta) => {
                    if meta.len() > 500_000 {
                        return ignore::WalkState::Continue;
                    }
                    meta.len()
                }
                Err(_) => return ignore::WalkState::Continue,
            };
            if super::is_minified_filename(path) {
                return ignore::WalkState::Continue;
            }
            let Some(bytes) = super::read_file_bytes(path, file_size) else {
                return ignore::WalkState::Continue;
            };

            // Single-pass any-of gate: find which queries hit this file.
            let mut hit_mask = vec![false; queries.len()];
            let mut any_hit = false;
            for m in ac.find_iter(&bytes[..]) {
                hit_mask[m.pattern().as_usize()] = true;
                any_hit = true;
            }
            if !any_hit {
                return ignore::WalkState::Continue;
            }

            if file_size >= super::MINIFIED_CHECK_THRESHOLD && super::looks_minified(&bytes) {
                return ignore::WalkState::Continue;
            }
            let Ok(content) = std::str::from_utf8(&bytes) else {
                return ignore::WalkState::Continue;
            };

            let (file_lines, mtime) = file_metadata(path);
            let file_type = detect_file_type(path);
            let lang = match file_type {
                FileType::Code(l) => Some(l),
                _ => None,
            };
            let ts_language = lang.and_then(outline_language);

            // Per-file local buckets so we lock global mutex once.
            let mut local: Vec<Vec<Match>> = vec![Vec::new(); queries.len()];

            if let Some(ref ts_lang) = ts_language {
                // Parse once, walk once per query that hit (cheap: walk is fast vs parse).
                for (i, q) in queries.iter().enumerate() {
                    if !hit_mask[i] {
                        continue;
                    }
                    let defs =
                        find_defs_treesitter(path, q, ts_lang, content, file_lines, mtime, cache);
                    if !defs.is_empty() {
                        local[i] = defs;
                    }
                }
            } else {
                for (i, q) in queries.iter().enumerate() {
                    if !hit_mask[i] {
                        continue;
                    }
                    let defs = find_defs_heuristic_buf(path, q, content, file_lines, mtime);
                    if !defs.is_empty() {
                        local[i] = defs;
                    }
                }
            }

            let any_local = local.iter().any(|v| !v.is_empty());
            if any_local {
                let mut all = buckets
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for (i, v) in local.into_iter().enumerate() {
                    if !v.is_empty() {
                        all[i].extend(v);
                    }
                }
            }

            ignore::WalkState::Continue
        })
    });

    Ok(buckets
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
}

fn find_usages_batch(
    queries: &[&str],
    matcher: &RegexMatcher,
    scope: &Path,
    glob: Option<&str>,
) -> Result<Vec<Vec<Match>>, TilthError> {
    let buckets: Mutex<Vec<Vec<Match>>> = Mutex::new(vec![Vec::new(); queries.len()]);
    let walker = super::walker(scope, glob)?;

    walker.run(|| {
        let buckets = &buckets;
        Box::new(move |entry| {
            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }
            let path = entry.path();
            if let Ok(meta) = std::fs::metadata(path) {
                if meta.len() > 500_000 {
                    return ignore::WalkState::Continue;
                }
            }
            let (file_lines, mtime) = file_metadata(path);

            // Per-file local buckets.
            let mut local: Vec<Vec<Match>> = vec![Vec::new(); queries.len()];
            let mut searcher = Searcher::new();
            let _ = searcher.search_path(
                matcher,
                path,
                UTF8(|line_num, line| {
                    // Dispatch line to whichever query has a word-boundary match.
                    // The regex already guaranteed at least one query matches; we
                    // re-check per-query so substrings inside larger words don't
                    // leak into the wrong bucket (e.g. "parseFoo" must not count
                    // toward "parse" when only "format" actually matched).
                    let bytes = line.as_bytes();
                    for (i, q) in queries.iter().enumerate() {
                        let qb = q.as_bytes();
                        let mut start = 0;
                        let mut hit = false;
                        while let Some(pos) = memchr::memmem::find(&bytes[start..], qb) {
                            let abs = start + pos;
                            let before_ok = abs == 0 || !is_word_byte(bytes[abs - 1]);
                            let after = abs + qb.len();
                            let after_ok = after >= bytes.len() || !is_word_byte(bytes[after]);
                            if before_ok && after_ok {
                                hit = true;
                                break;
                            }
                            start = abs + 1;
                        }
                        if hit {
                            local[i].push(Match {
                                path: path.to_path_buf(),
                                line: line_num as u32,
                                text: line.trim_end().to_string(),
                                is_definition: false,
                                exact: true,
                                file_lines,
                                mtime,
                                def_range: None,
                                def_name: None,
                                def_weight: 0,
                                impl_target: None,
                            });
                        }
                    }
                    Ok(true)
                }),
            );

            let any_local = local.iter().any(|v| !v.is_empty());
            if any_local {
                let mut all = buckets
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for (i, v) in local.into_iter().enumerate() {
                    if !v.is_empty() {
                        all[i].extend(v);
                    }
                }
            }
            ignore::WalkState::Continue
        })
    });

    Ok(buckets
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
}

/// Symbol search: find definitions via tree-sitter, usages via ripgrep, concurrently.
/// Merge results, deduplicate, definitions first.
pub fn search(
    query: &str,
    scope: &Path,
    cache: Option<&crate::cache::OutlineCache>,
    context: Option<&Path>,
    glob: Option<&str>,
) -> Result<SearchResult, TilthError> {
    // Compile regex once, share across both arms
    let word_pattern = format!(r"\b{}\b", regex_syntax::escape(query));
    let matcher = RegexMatcher::new(&word_pattern).map_err(|e| TilthError::InvalidQuery {
        query: query.to_string(),
        reason: e.to_string(),
    })?;

    let (defs, usages) = rayon::join(
        || find_definitions(query, scope, glob, cache),
        || find_usages(query, &matcher, scope, glob),
    );

    let defs = defs?;
    let usages = usages?;

    // Deduplicate: remove usage matches that overlap with definition matches.
    // Linear scan — max ~30 defs from EARLY_QUIT_THRESHOLD, no allocation needed.
    let mut merged: Vec<Match> = defs;
    let def_count = merged.len();

    for m in usages {
        let dominated = merged[..def_count]
            .iter()
            .any(|d| d.path == m.path && d.line == m.line);
        if !dominated {
            merged.push(m);
        }
    }

    let total = merged.len();
    let usage_count = total - def_count;

    rank::sort(&mut merged, query, scope, context);

    Ok(SearchResult {
        query: query.to_string(),
        scope: scope.to_path_buf(),
        matches: merged,
        total_found: total,
        definitions: def_count,
        usages: usage_count,
        has_more: false,
        offset: 0,
    })
}

/// Find definitions using tree-sitter structural detection.
/// For each file containing the query string, parse with tree-sitter and walk
/// definition nodes to see if any declare the queried symbol.
/// Falls back to keyword heuristic for files without grammars.
///
/// Single-read design: reads each file once, checks for symbol via
/// `memchr::memmem` (SIMD), then reuses the buffer for tree-sitter parsing.
/// Early termination: quits the parallel walker once enough defs are found.
fn find_definitions(
    query: &str,
    scope: &Path,
    glob: Option<&str>,
    cache: Option<&crate::cache::OutlineCache>,
) -> Result<Vec<Match>, TilthError> {
    let matches: Mutex<Vec<Match>> = Mutex::new(Vec::new());
    // Relaxed is correct: walker.run() joins all threads before we read the final value.
    // Early-quit checks are approximate by design — one extra iteration is harmless.
    let found_count = AtomicUsize::new(0);
    let needle = query.as_bytes();

    let walker = super::walker(scope, glob)?;

    walker.run(|| {
        let matches = &matches;
        let found_count = &found_count;
        let cache = cache;

        Box::new(move |entry| {
            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };

            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }

            let path = entry.path();

            // Skip oversized files — avoid tree-sitter parsing multi-MB minified bundles
            let file_size = match std::fs::metadata(path) {
                Ok(meta) => {
                    if meta.len() > 500_000 {
                        return ignore::WalkState::Continue;
                    }
                    meta.len()
                }
                Err(_) => return ignore::WalkState::Continue,
            };

            // Skip minified/bundled assets by filename — they're parseable but
            // tree-sitter costs 100-500ms on them with zero useful defs.
            if super::is_minified_filename(path) {
                return ignore::WalkState::Continue;
            }

            // Fast byte-level scan: mmap (or heap-read for tiny files) +
            // memchr SIMD search. Skips UTF-8 validation on ~90% of files
            // that don't contain the symbol.
            let Some(bytes) = super::read_file_bytes(path, file_size) else {
                return ignore::WalkState::Continue;
            };

            if memchr::memmem::find(&bytes, needle).is_none() {
                return ignore::WalkState::Continue;
            }

            // Content-based minified detection for large files that slipped
            // through filename check (e.g. `app.js` actually minified).
            if file_size >= super::MINIFIED_CHECK_THRESHOLD && super::looks_minified(&bytes) {
                return ignore::WalkState::Continue;
            }

            // Hit: validate UTF-8 only now (matched files are <10% in typical search)
            let Ok(content) = std::str::from_utf8(&bytes) else {
                return ignore::WalkState::Continue;
            };

            // Get file metadata once per file
            let (file_lines, mtime) = file_metadata(path);

            // Try tree-sitter structural detection
            let file_type = detect_file_type(path);
            let lang = match file_type {
                FileType::Code(l) => Some(l),
                _ => None,
            };

            let ts_language = lang.and_then(outline_language);

            let mut file_defs = if let Some(ref ts_lang) = ts_language {
                find_defs_treesitter(path, query, ts_lang, &content, file_lines, mtime, cache)
            } else {
                Vec::new()
            };

            // Fallback: keyword heuristic for files without grammars
            if file_defs.is_empty() && ts_language.is_none() {
                file_defs = find_defs_heuristic_buf(path, query, &content, file_lines, mtime);
            }

            if !file_defs.is_empty() {
                found_count.fetch_add(file_defs.len(), Ordering::Relaxed);
                let mut all = matches
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                all.extend(file_defs);
            }

            ignore::WalkState::Continue
        })
    });

    Ok(matches
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
}

/// Tree-sitter structural definition detection.
/// Accepts pre-read content — no redundant file read.
fn find_defs_treesitter(
    path: &Path,
    query: &str,
    ts_lang: &tree_sitter::Language,
    content: &str,
    file_lines: u32,
    mtime: SystemTime,
    cache: Option<&crate::cache::OutlineCache>,
) -> Vec<Match> {
    let tree = match cache {
        Some(c) => match c.get_or_parse(path, mtime, content, ts_lang) {
            Some(t) => t,
            None => return Vec::new(),
        },
        None => {
            let mut parser = tree_sitter::Parser::new();
            if parser.set_language(ts_lang).is_err() {
                return Vec::new();
            }
            let Some(tree) = parser.parse(content, None) else {
                return Vec::new();
            };
            tree
        }
    };

    let lines: Vec<&str> = content.lines().collect();
    let root = tree.root_node();
    let mut defs = Vec::new();

    walk_for_definitions(root, query, path, &lines, file_lines, mtime, &mut defs, 0);

    defs
}

/// Recursively walk AST nodes looking for definitions of the queried symbol.
fn walk_for_definitions(
    node: tree_sitter::Node,
    query: &str,
    path: &Path,
    lines: &[&str],
    file_lines: u32,
    mtime: SystemTime,
    defs: &mut Vec<Match>,
    depth: usize,
) {
    if depth > 3 {
        return;
    }

    let kind = node.kind();

    if DEFINITION_KINDS.contains(&kind) {
        // Check if this node defines the queried symbol
        if let Some(name) = extract_definition_name(node, lines) {
            if name == query {
                let line_num = node.start_position().row as u32 + 1;
                let line_text = lines
                    .get(node.start_position().row)
                    .unwrap_or(&"")
                    .trim_end();
                defs.push(Match {
                    path: path.to_path_buf(),
                    line: line_num,
                    text: line_text.to_string(),
                    is_definition: true,
                    exact: true,
                    file_lines,
                    mtime,
                    def_range: Some((
                        node.start_position().row as u32 + 1,
                        node.end_position().row as u32 + 1,
                    )),
                    def_name: Some(query.to_string()),
                    def_weight: definition_weight(node.kind()),
                    impl_target: None,
                });
            }
        }

        // Impl/interface detection: surface `impl Trait for Type` and
        // `class X implements Interface` blocks when searching for the trait/interface.
        if kind == "impl_item" {
            if let Some(trait_name) = extract_impl_trait(node, lines) {
                if trait_name == query {
                    let impl_type =
                        extract_impl_type(node, lines).unwrap_or_else(|| "<unknown>".to_string());
                    let line_num = node.start_position().row as u32 + 1;
                    let line_text = lines
                        .get(node.start_position().row)
                        .unwrap_or(&"")
                        .trim_end();
                    defs.push(Match {
                        path: path.to_path_buf(),
                        line: line_num,
                        text: line_text.to_string(),
                        is_definition: true,
                        exact: true,
                        file_lines,
                        mtime,
                        def_range: Some((
                            node.start_position().row as u32 + 1,
                            node.end_position().row as u32 + 1,
                        )),
                        def_name: Some(format!("impl {query} for {impl_type}")),
                        def_weight: 80,
                        impl_target: Some(query.to_string()),
                    });
                }
            }
        } else if kind == "class_declaration" || kind == "class_definition" {
            let interfaces = extract_implemented_interfaces(node, lines);
            if interfaces.iter().any(|i| i == query) {
                let class_name = extract_definition_name(node, lines)
                    .unwrap_or_else(|| "<anonymous>".to_string());
                let line_num = node.start_position().row as u32 + 1;
                let line_text = lines
                    .get(node.start_position().row)
                    .unwrap_or(&"")
                    .trim_end();
                defs.push(Match {
                    path: path.to_path_buf(),
                    line: line_num,
                    text: line_text.to_string(),
                    is_definition: true,
                    exact: true,
                    file_lines,
                    mtime,
                    def_range: Some((
                        node.start_position().row as u32 + 1,
                        node.end_position().row as u32 + 1,
                    )),
                    def_name: Some(format!("{class_name} implements {query}")),
                    def_weight: 80,
                    impl_target: Some(query.to_string()),
                });
            }
        }
    }

    // Recurse into children (for nested definitions, class bodies, impl blocks, etc.)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_for_definitions(
            child,
            query,
            path,
            lines,
            file_lines,
            mtime,
            defs,
            depth + 1,
        );
    }
}

/// Keyword heuristic fallback for files without tree-sitter grammars.
/// Operates on pre-read buffer — no redundant file read.
fn find_defs_heuristic_buf(
    path: &Path,
    query: &str,
    content: &str,
    file_lines: u32,
    mtime: SystemTime,
) -> Vec<Match> {
    let mut defs = Vec::new();

    for (i, line) in content.lines().enumerate() {
        if line.contains(query) && is_definition_line(line) {
            defs.push(Match {
                path: path.to_path_buf(),
                line: (i + 1) as u32,
                text: line.trim_end().to_string(),
                is_definition: true,
                exact: true,
                file_lines,
                mtime,
                def_range: None,
                def_name: Some(query.to_string()),
                def_weight: 60,
                impl_target: None,
            });
        }
    }

    defs
}

/// Find all usages via ripgrep (word-boundary matching).
/// Collects per-file, locks once per file (not per line).
/// Early termination once enough usages found.
fn find_usages(
    query: &str,
    matcher: &RegexMatcher,
    scope: &Path,
    glob: Option<&str>,
) -> Result<Vec<Match>, TilthError> {
    let matches: Mutex<Vec<Match>> = Mutex::new(Vec::new());
    // Relaxed: same reasoning as find_definitions — approximate early-quit, joined before read
    let found_count = AtomicUsize::new(0);

    let walker = super::walker(scope, glob)?;

    walker.run(|| {
        let matches = &matches;
        let found_count = &found_count;

        Box::new(move |entry| {
            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };

            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }

            let path = entry.path();

            // Skip oversized files
            if let Ok(meta) = std::fs::metadata(path) {
                if meta.len() > 500_000 {
                    return ignore::WalkState::Continue;
                }
            }

            let (file_lines, mtime) = file_metadata(path);

            let mut file_matches = Vec::new();
            let mut searcher = Searcher::new();

            let _ = searcher.search_path(
                matcher,
                path,
                UTF8(|line_num, line| {
                    file_matches.push(Match {
                        path: path.to_path_buf(),
                        line: line_num as u32,
                        text: line.trim_end().to_string(),
                        is_definition: false,
                        exact: line.contains(query),
                        file_lines,
                        mtime,
                        def_range: None,
                        def_name: None,
                        def_weight: 0,
                        impl_target: None,
                    });
                    Ok(true)
                }),
            );

            if !file_matches.is_empty() {
                found_count.fetch_add(file_matches.len(), Ordering::Relaxed);
                let mut all = matches
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                all.extend(file_matches);
            }

            ignore::WalkState::Continue
        })
    });

    Ok(matches
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Keyword heuristic fallback — only used when tree-sitter grammar unavailable.
fn is_definition_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("fn ")
        || trimmed.starts_with("pub fn ")
        || trimmed.starts_with("pub(crate) fn ")
        || trimmed.starts_with("async fn ")
        || trimmed.starts_with("pub async fn ")
        || trimmed.starts_with("function ")
        || trimmed.starts_with("export function ")
        || trimmed.starts_with("export default function ")
        || trimmed.starts_with("export async function ")
        || trimmed.starts_with("async function ")
        || trimmed.starts_with("const ")
        || trimmed.starts_with("export const ")
        || trimmed.starts_with("let ")
        || trimmed.starts_with("export let ")
        || trimmed.starts_with("var ")
        || trimmed.starts_with("export var ")
        || trimmed.starts_with("class ")
        || trimmed.starts_with("export class ")
        || trimmed.starts_with("interface ")
        || trimmed.starts_with("export interface ")
        || trimmed.starts_with("type ")
        || trimmed.starts_with("export type ")
        || trimmed.starts_with("struct ")
        || trimmed.starts_with("pub struct ")
        || trimmed.starts_with("enum ")
        || trimmed.starts_with("pub enum ")
        || trimmed.starts_with("trait ")
        || trimmed.starts_with("pub trait ")
        || trimmed.starts_with("impl ")
        || trimmed.starts_with("def ")
        || trimmed.starts_with("async def ")
        || trimmed.starts_with("func ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn rust_definitions_detected() {
        let code = r#"pub fn hello(name: &str) -> String {
    format!("Hello, {}", name)
}

pub struct Foo {
    bar: i32,
}

pub(crate) fn dispatch_tool(tool: &str) -> Result<String, String> {
    match tool {
        "read" => Ok("read".to_string()),
        _ => Err("unknown".to_string()),
    }
}
"#;
        let ts_lang = crate::lang::outline::outline_language(crate::types::Lang::Rust).unwrap();

        let defs = find_defs_treesitter(
            std::path::Path::new("test.rs"),
            "hello",
            &ts_lang,
            code,
            15,
            SystemTime::now(),
            None,
        );
        assert!(!defs.is_empty(), "should find 'hello' definition");
        assert!(defs[0].is_definition);
        assert!(defs[0].def_range.is_some());

        let defs = find_defs_treesitter(
            std::path::Path::new("test.rs"),
            "Foo",
            &ts_lang,
            code,
            15,
            SystemTime::now(),
            None,
        );
        assert!(!defs.is_empty(), "should find 'Foo' definition");

        let defs = find_defs_treesitter(
            std::path::Path::new("test.rs"),
            "dispatch_tool",
            &ts_lang,
            code,
            15,
            SystemTime::now(),
            None,
        );
        assert!(!defs.is_empty(), "should find 'dispatch_tool' definition");
    }
}
