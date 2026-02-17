use std::collections::HashSet;
use std::path::{Path, PathBuf};

use streaming_iterator::StreamingIterator;

use crate::cache::OutlineCache;
use crate::read::outline::code::outline_language;
use crate::types::{Lang, OutlineEntry};

/// A resolved callee: a function/method called from within an expanded definition.
#[derive(Debug)]
pub struct ResolvedCallee {
    pub name: String,
    pub file: PathBuf,
    pub start_line: u32,
    pub end_line: u32,
    pub signature: Option<String>,
}

/// A resolved callee with its own callees (2nd hop).
#[derive(Debug)]
pub struct ResolvedCalleeNode {
    pub callee: ResolvedCallee,
    /// 2nd-hop callees resolved from within this callee's body.
    pub children: Vec<ResolvedCallee>,
}

/// Return the tree-sitter query string for extracting callee names in the given language.
/// Each language has patterns targeting `@callee` captures on call-like expressions.
pub(crate) fn callee_query_str(lang: Lang) -> Option<&'static str> {
    match lang {
        Lang::Rust => Some(concat!(
            "(call_expression function: (identifier) @callee)\n",
            "(call_expression function: (field_expression field: (field_identifier) @callee))\n",
            "(call_expression function: (scoped_identifier name: (identifier) @callee))\n",
            "(macro_invocation macro: (identifier) @callee)\n",
        )),
        Lang::Go => Some(concat!(
            "(call_expression function: (identifier) @callee)\n",
            "(call_expression function: (selector_expression field: (field_identifier) @callee))\n",
        )),
        Lang::Python => Some(concat!(
            "(call function: (identifier) @callee)\n",
            "(call function: (attribute attribute: (identifier) @callee))\n",
        )),
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => Some(concat!(
            "(call_expression function: (identifier) @callee)\n",
            "(call_expression function: (member_expression property: (property_identifier) @callee))\n",
        )),
        Lang::Java => Some(
            "(method_invocation name: (identifier) @callee)\n",
        ),
        Lang::C | Lang::Cpp => Some(concat!(
            "(call_expression function: (identifier) @callee)\n",
            "(call_expression function: (field_expression field: (field_identifier) @callee))\n",
        )),
        Lang::Ruby => Some(
            "(call method: (identifier) @callee)\n",
        ),
        _ => None,
    }
}

/// Extract names of functions/methods called within a given line range.
/// Uses tree-sitter query patterns to find call expressions.
///
/// If `def_range` is `Some((start, end))`, only callees whose match position
/// falls within lines `start..=end` (1-indexed) are returned.
/// Returns a deduplicated, sorted list of callee names.
pub fn extract_callee_names(
    content: &str,
    lang: Lang,
    def_range: Option<(u32, u32)>,
) -> Vec<String> {
    let Some(ts_lang) = outline_language(lang) else {
        return Vec::new();
    };

    let Some(query_str) = callee_query_str(lang) else {
        return Vec::new();
    };

    // Compile the query — if the grammar doesn't support these patterns, bail gracefully.
    let Ok(query) = tree_sitter::Query::new(&ts_lang, query_str) else {
        return Vec::new();
    };

    let Some(callee_idx) = query.capture_index_for_name("callee") else {
        return Vec::new();
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return Vec::new();
    }

    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };

    let content_bytes = content.as_bytes();
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content_bytes);

    let mut names: Vec<String> = Vec::new();

    while let Some(m) = matches.next() {
        for cap in m.captures {
            if cap.index != callee_idx {
                continue;
            }

            // 1-indexed line number of the capture
            let line = cap.node.start_position().row as u32 + 1;

            // Filter by def_range if provided
            if let Some((start, end)) = def_range {
                if line < start || line > end {
                    continue;
                }
            }

            if let Ok(text) = cap.node.utf8_text(content_bytes) {
                let name = text.to_string();
                names.push(name);
            }
        }
    }

    names.sort();
    names.dedup();
    names
}

/// Get structured outline entries for file content.
pub fn get_outline_entries(content: &str, lang: Lang) -> Vec<OutlineEntry> {
    let Some(ts_lang) = outline_language(lang) else {
        return Vec::new();
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return Vec::new();
    }

    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };

    let lines: Vec<&str> = content.lines().collect();
    crate::read::outline::code::walk_top_level(tree.root_node(), &lines, lang)
}

/// Match callee names against outline entries, moving resolved names out of `remaining`.
fn resolve_from_entries(
    entries: &[OutlineEntry],
    file_path: &Path,
    remaining: &mut std::collections::HashSet<&str>,
    resolved: &mut Vec<ResolvedCallee>,
) {
    for entry in entries {
        // Check top-level entry name
        if remaining.contains(entry.name.as_str()) {
            remaining.remove(entry.name.as_str());
            resolved.push(ResolvedCallee {
                name: entry.name.clone(),
                file: file_path.to_path_buf(),
                start_line: entry.start_line,
                end_line: entry.end_line,
                signature: entry.signature.clone(),
            });
        }

        // Check children (methods in classes/impl blocks)
        for child in &entry.children {
            if remaining.contains(child.name.as_str()) {
                remaining.remove(child.name.as_str());
                resolved.push(ResolvedCallee {
                    name: child.name.clone(),
                    file: file_path.to_path_buf(),
                    start_line: child.start_line,
                    end_line: child.end_line,
                    signature: child.signature.clone(),
                });
            }
        }

        if remaining.is_empty() {
            return;
        }
    }
}

/// Resolve callee names to their definition locations.
///
/// Strategy: check the source file's own outline first (cheapest), then scan
/// imported files resolved from the source's import statements.
pub fn resolve_callees(
    callee_names: &[String],
    source_path: &Path,
    source_content: &str,
    _cache: &OutlineCache,
    bloom: &crate::index::bloom::BloomFilterCache,
) -> Vec<ResolvedCallee> {
    if callee_names.is_empty() {
        return Vec::new();
    }

    let file_type = crate::read::detect_file_type(source_path);
    let crate::types::FileType::Code(lang) = file_type else {
        return Vec::new();
    };

    let mut remaining: std::collections::HashSet<&str> =
        callee_names.iter().map(String::as_str).collect();
    let mut resolved = Vec::new();

    // 1. Check source file's own outline entries
    let entries = get_outline_entries(source_content, lang);
    resolve_from_entries(&entries, source_path, &mut remaining, &mut resolved);

    if remaining.is_empty() {
        return resolved;
    }

    // 2. Check imported files
    let imported =
        crate::read::imports::resolve_related_files_with_content(source_path, source_content);

    for import_path in imported {
        if remaining.is_empty() {
            break;
        }

        // Read file content once for both bloom check and parsing
        let Ok(import_content) = std::fs::read_to_string(&import_path) else {
            continue;
        };

        // Get mtime for bloom cache
        let mtime = std::fs::metadata(&import_path)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

        // Bloom pre-filter: check if ANY of the remaining symbols might be in this file
        let mut might_have_any = false;
        for name in &remaining {
            if bloom.contains(&import_path, mtime, &import_content, name) {
                might_have_any = true;
                break;
            }
        }

        if !might_have_any {
            // Bloom filter says none of the symbols are in this file
            continue;
        }

        let import_type = crate::read::detect_file_type(&import_path);
        let crate::types::FileType::Code(import_lang) = import_type else {
            continue;
        };

        let import_entries = get_outline_entries(&import_content, import_lang);
        resolve_from_entries(&import_entries, &import_path, &mut remaining, &mut resolved);
    }

    if remaining.is_empty() {
        return resolved;
    }

    // 3. For Go: scan same-directory files (same package, no explicit imports)
    if lang == Lang::Go {
        resolve_same_package(&mut remaining, &mut resolved, source_path);
    }

    resolved
}

/// Go same-package resolution: scan .go files in the same directory.
///
/// Go packages are directory-scoped — all .go files in a directory share the
/// same namespace without explicit imports. This resolves callees like
/// `safeInt8` in `context.go` that are defined in `utils.go`.
fn resolve_same_package(
    remaining: &mut std::collections::HashSet<&str>,
    resolved: &mut Vec<ResolvedCallee>,
    source_path: &Path,
) {
    const MAX_FILES: usize = 20;
    const MAX_FILE_SIZE: u64 = 100_000; // 100KB

    let Some(dir) = source_path.parent() else {
        return;
    };

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    // Collect eligible .go files, sorted for deterministic order
    let mut go_files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| {
            let path = e.path();
            let name = e.file_name();
            let name_str = name.to_string_lossy();
            path != source_path
                && name_str.ends_with(".go")
                && !name_str.ends_with("_test.go")
                && e.metadata().is_ok_and(|m| m.len() <= MAX_FILE_SIZE)
        })
        .map(|e| e.path())
        .collect();

    go_files.sort();
    go_files.truncate(MAX_FILES);

    for go_path in go_files {
        if remaining.is_empty() {
            break;
        }

        let Ok(content) = std::fs::read_to_string(&go_path) else {
            continue;
        };

        let outline = get_outline_entries(&content, Lang::Go);
        resolve_from_entries(&outline, &go_path, remaining, resolved);
    }
}

/// Resolve callees transitively up to `depth_limit` hops with budget cap.
///
/// First hop uses `resolve_callees()` on the source content. For each resolved
/// callee at depth < `depth_limit`, reads the callee's file, extracts nested
/// callee names from the definition range, and resolves them as children.
///
/// `budget` caps the total number of 2nd-hop (child) callees across all parents.
/// Cycle detection prevents infinite loops via `(file, start_line)` tracking.
pub fn resolve_callees_transitive(
    initial_names: &[String],
    source_path: &Path,
    source_content: &str,
    cache: &OutlineCache,
    bloom: &crate::index::bloom::BloomFilterCache,
    depth_limit: u32,
    budget: usize,
) -> Vec<ResolvedCalleeNode> {
    // 1st hop: resolve direct callees (existing logic)
    let first_hop = resolve_callees(initial_names, source_path, source_content, cache, bloom);

    if depth_limit < 2 || first_hop.is_empty() {
        return first_hop
            .into_iter()
            .map(|c| ResolvedCalleeNode {
                callee: c,
                children: Vec::new(),
            })
            .collect();
    }

    // Cycle detection: track visited (file, start_line) pairs
    let mut visited: HashSet<(PathBuf, u32)> = HashSet::new();

    // Mark all 1st-hop callees as visited
    for c in &first_hop {
        visited.insert((c.file.clone(), c.start_line));
    }

    let mut budget_remaining = budget;
    let mut result = Vec::with_capacity(first_hop.len());

    for parent in first_hop {
        let children = if budget_remaining > 0 {
            resolve_second_hop(&parent, cache, bloom, &mut visited, &mut budget_remaining)
        } else {
            Vec::new()
        };
        result.push(ResolvedCalleeNode {
            callee: parent,
            children,
        });
    }

    result
}

/// Resolve 2nd-hop callees for a single parent callee.
fn resolve_second_hop(
    parent: &ResolvedCallee,
    cache: &OutlineCache,
    bloom: &crate::index::bloom::BloomFilterCache,
    visited: &mut HashSet<(PathBuf, u32)>,
    budget: &mut usize,
) -> Vec<ResolvedCallee> {
    let file_type = crate::read::detect_file_type(&parent.file);
    let crate::types::FileType::Code(lang) = file_type else {
        return Vec::new();
    };

    let Ok(content) = std::fs::read_to_string(&parent.file) else {
        return Vec::new();
    };

    let def_range = Some((parent.start_line, parent.end_line));
    let nested_names = extract_callee_names(&content, lang, def_range);

    if nested_names.is_empty() {
        return Vec::new();
    }

    let mut resolved = resolve_callees(&nested_names, &parent.file, &content, cache, bloom);

    // Filter: skip self-recursive calls and already-visited callees
    resolved.retain(|c| {
        let key = (c.file.clone(), c.start_line);
        // Skip if same definition as parent
        if c.file == parent.file && c.start_line == parent.start_line {
            return false;
        }
        // Skip if already visited (cycle detection)
        if visited.contains(&key) {
            return false;
        }
        true
    });

    // Apply budget cap
    if resolved.len() > *budget {
        resolved.truncate(*budget);
    }

    // Mark children as visited and decrement budget
    for c in &resolved {
        visited.insert((c.file.clone(), c.start_line));
    }
    *budget = budget.saturating_sub(resolved.len());

    resolved
}
