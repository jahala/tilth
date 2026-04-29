use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use streaming_iterator::StreamingIterator;

use crate::lang::treesitter::{
    extract_definition_name, extract_elixir_definition_name, is_elixir_definition,
    node_text_simple, DEFINITION_KINDS,
};

use crate::cache::OutlineCache;
use crate::error::TilthError;
use crate::lang::detect_file_type;
use crate::lang::outline::outline_language;
use crate::session::Session;
use crate::types::FileType;

const MAX_MATCHES: usize = 10;
/// Stop walking once we have this many raw matches. Generous headroom for dedup + ranking.
const EARLY_QUIT_THRESHOLD: usize = 30;
/// Max unique caller functions to trace for 2nd hop. Above this = wide fan-out, skip.
const IMPACT_FANOUT_THRESHOLD: usize = 10;
/// Max 2nd-hop results to display.
const IMPACT_MAX_RESULTS: usize = 15;
/// Early quit for batch caller search.
const BATCH_EARLY_QUIT: usize = 50;

/// A single caller match — a call site of a target symbol.
#[derive(Debug)]
pub struct CallerMatch {
    pub path: PathBuf,
    pub line: u32,
    pub calling_function: String,
    pub call_text: String,
    /// Line range of the calling function (for expand).
    pub caller_range: Option<(u32, u32)>,
    /// File content, already read during `find_callers` — avoids re-reading during expand.
    /// Shared across all call sites in the same file via reference counting.
    pub content: Arc<String>,
}

/// Find all call sites of a target symbol across the codebase using tree-sitter.
/// Walk the scope once, return all direct call sites of `target` plus
/// whether the literal name appeared anywhere (used to distinguish
/// "real symbol with no direct callers" from "typo'd query" — the hint
/// shown to the agent differs between those cases).
pub fn find_callers(
    target: &str,
    scope: &Path,
    bloom: &crate::index::bloom::BloomFilterCache,
    glob: Option<&str>,
) -> Result<(Vec<CallerMatch>, bool), TilthError> {
    let matches: Mutex<Vec<CallerMatch>> = Mutex::new(Vec::new());
    let found_count = AtomicUsize::new(0);
    let target_seen = AtomicBool::new(false);
    let needle = target.as_bytes();

    let walker = super::walker(scope, glob)?;

    walker.run(|| {
        let matches = &matches;
        let found_count = &found_count;
        let target_seen = &target_seen;

        Box::new(move |entry| {
            // Early termination: enough callers found
            if found_count.load(Ordering::Relaxed) >= EARLY_QUIT_THRESHOLD {
                return ignore::WalkState::Quit;
            }

            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };

            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }

            let path = entry.path();

            // Single metadata call: check size and capture mtime together
            let (file_len, mtime) = match std::fs::metadata(path) {
                Ok(meta) => (
                    meta.len(),
                    meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                ),
                Err(_) => return ignore::WalkState::Continue,
            };
            if file_len > 500_000 {
                // Oversized files are not parsed, but we still need to know
                // whether the literal target appears anywhere in scope so the
                // "no callers" message can distinguish "real symbol, indirect
                // dispatch" from "typo, doesn't exist." mmap is lazy, so the
                // scan only touches pages that contain the needle prefix.
                if !target_seen.load(Ordering::Relaxed) {
                    if let Ok(file) = std::fs::File::open(path) {
                        if let Ok(mmap) = unsafe { memmap2::Mmap::map(&file) } {
                            if memchr::memmem::find(&mmap, needle).is_some() {
                                target_seen.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                }
                return ignore::WalkState::Continue;
            }

            // Single read: read file once, use buffer for both check and parse
            let Ok(content) = fs::read_to_string(path) else {
                return ignore::WalkState::Continue;
            };

            // Bloom pre-filter: skip if target is definitely not in file
            if !bloom.contains(path, mtime, &content, target) {
                return ignore::WalkState::Continue;
            }

            // Fast byte check via memchr::memmem (SIMD) — skip files without the symbol
            if memchr::memmem::find(content.as_bytes(), needle).is_none() {
                return ignore::WalkState::Continue;
            }

            // Symbol literal exists in this file (whether or not it's a call site).
            target_seen.store(true, Ordering::Relaxed);

            // Only process files with tree-sitter grammars
            let file_type = detect_file_type(path);
            let FileType::Code(lang) = file_type else {
                return ignore::WalkState::Continue;
            };

            let Some(ts_lang) = outline_language(lang) else {
                return ignore::WalkState::Continue;
            };

            let file_callers = find_callers_treesitter(path, target, &ts_lang, &content, lang);

            if !file_callers.is_empty() {
                found_count.fetch_add(file_callers.len(), Ordering::Relaxed);
                let mut all = matches
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                all.extend(file_callers);
            }

            ignore::WalkState::Continue
        })
    });

    let callers = matches
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Relaxed is sufficient: walker.run() uses thread::scope under the hood,
    // and the join at the end of run() establishes happens-before between
    // every worker store and this load. No Acquire/Release pair needed.
    Ok((callers, target_seen.load(Ordering::Relaxed)))
}

/// Tree-sitter call site detection.
fn find_callers_treesitter(
    path: &Path,
    target: &str,
    ts_lang: &tree_sitter::Language,
    content: &str,
    lang: crate::types::Lang,
) -> Vec<CallerMatch> {
    // Get the query string for this language
    let Some(query_str) = super::callees::callee_query_str(lang) else {
        return Vec::new();
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(ts_lang).is_err() {
        return Vec::new();
    }

    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };

    let content_bytes = content.as_bytes();
    let lines: Vec<&str> = content.lines().collect();

    // One Arc per file — all call sites share the same allocation.
    let shared_content: Arc<String> = Arc::new(content.to_string());

    let Some(callers) = super::callees::with_callee_query(ts_lang, query_str, |query| {
        let Some(callee_idx) = query.capture_index_for_name("callee") else {
            return Vec::new();
        };

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), content_bytes);
        let mut callers = Vec::new();

        while let Some(m) = matches.next() {
            for cap in m.captures {
                if cap.index != callee_idx {
                    continue;
                }

                // Check if the captured text matches our target symbol
                let Ok(text) = cap.node.utf8_text(content_bytes) else {
                    continue;
                };

                if text != target {
                    continue;
                }

                // Found a call site! Now walk up to find the calling function
                let line = cap.node.start_position().row as u32 + 1;

                // Get the call text (the whole call expression, not just the callee)
                let call_node = cap.node.parent().unwrap_or(cap.node);
                let same_line = call_node.start_position().row == call_node.end_position().row;
                let call_text: String = if same_line {
                    let row = call_node.start_position().row;
                    if row < lines.len() {
                        lines[row].trim().to_string()
                    } else {
                        text.to_string()
                    }
                } else {
                    text.to_string()
                };

                // Walk up the tree to find the enclosing function
                let (calling_function, caller_range) =
                    find_enclosing_function(cap.node, &lines, lang);

                callers.push(CallerMatch {
                    path: path.to_path_buf(),
                    line,
                    calling_function,
                    call_text,
                    caller_range,
                    content: Arc::clone(&shared_content),
                });
            }
        }

        callers
    }) else {
        return Vec::new();
    };

    callers
}

/// Find all call sites of any symbol in `targets` across the codebase using a single walk.
/// Returns tuples of (`target_name`, match) so callers know which symbol was matched.
pub(crate) fn find_callers_batch(
    targets: &HashSet<String>,
    scope: &Path,
    bloom: &crate::index::bloom::BloomFilterCache,
    glob: Option<&str>,
) -> Result<Vec<(String, CallerMatch)>, TilthError> {
    let matches: Mutex<Vec<(String, CallerMatch)>> = Mutex::new(Vec::new());
    let found_count = AtomicUsize::new(0);

    let walker = super::walker(scope, glob)?;

    walker.run(|| {
        let matches = &matches;
        let found_count = &found_count;

        Box::new(move |entry| {
            // Early termination: enough callers found
            if found_count.load(Ordering::Relaxed) >= BATCH_EARLY_QUIT {
                return ignore::WalkState::Quit;
            }

            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };

            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }

            let path = entry.path();

            // Single metadata call: check size and capture mtime together
            let (file_len, mtime) = match std::fs::metadata(path) {
                Ok(meta) => (
                    meta.len(),
                    meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                ),
                Err(_) => return ignore::WalkState::Continue,
            };
            if file_len > 500_000 {
                return ignore::WalkState::Continue;
            }

            // Single read: read file once, use buffer for both check and parse
            let Ok(content) = fs::read_to_string(path) else {
                return ignore::WalkState::Continue;
            };

            // Bloom pre-filter: skip if none of the targets are definitely in the file
            if !targets
                .iter()
                .any(|t| bloom.contains(path, mtime, &content, t))
            {
                return ignore::WalkState::Continue;
            }

            // Fast byte check via memchr::memmem (SIMD) — skip files without any target symbol
            if !targets
                .iter()
                .any(|t| memchr::memmem::find(content.as_bytes(), t.as_bytes()).is_some())
            {
                return ignore::WalkState::Continue;
            }

            // Only process files with tree-sitter grammars
            let file_type = detect_file_type(path);
            let FileType::Code(lang) = file_type else {
                return ignore::WalkState::Continue;
            };

            let Some(ts_lang) = outline_language(lang) else {
                return ignore::WalkState::Continue;
            };

            let file_callers =
                find_callers_treesitter_batch(path, targets, &ts_lang, &content, lang);

            if !file_callers.is_empty() {
                found_count.fetch_add(file_callers.len(), Ordering::Relaxed);
                let mut all = matches
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                all.extend(file_callers);
            }

            ignore::WalkState::Continue
        })
    });

    Ok(matches
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
}

/// Tree-sitter call site detection for a set of target symbols.
/// Returns tuples of (`matched_target_name`, `CallerMatch`).
fn find_callers_treesitter_batch(
    path: &Path,
    targets: &HashSet<String>,
    ts_lang: &tree_sitter::Language,
    content: &str,
    lang: crate::types::Lang,
) -> Vec<(String, CallerMatch)> {
    // Get the query string for this language
    let Some(query_str) = super::callees::callee_query_str(lang) else {
        return Vec::new();
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(ts_lang).is_err() {
        return Vec::new();
    }

    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };

    let content_bytes = content.as_bytes();
    let lines: Vec<&str> = content.lines().collect();

    // One Arc per file — all call sites share the same allocation.
    let shared_content: Arc<String> = Arc::new(content.to_string());

    let Some(callers) = super::callees::with_callee_query(ts_lang, query_str, |query| {
        let Some(callee_idx) = query.capture_index_for_name("callee") else {
            return Vec::new();
        };

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), content_bytes);
        let mut callers = Vec::new();

        while let Some(m) = matches.next() {
            for cap in m.captures {
                if cap.index != callee_idx {
                    continue;
                }

                // Check if the captured text matches any of our target symbols
                let Ok(text) = cap.node.utf8_text(content_bytes) else {
                    continue;
                };

                if !targets.contains(text) {
                    continue;
                }

                let matched_target = text.to_string();

                // Found a call site! Now walk up to find the calling function
                let line = cap.node.start_position().row as u32 + 1;

                // Get the call text (the whole call expression, not just the callee)
                let call_node = cap.node.parent().unwrap_or(cap.node);
                let same_line = call_node.start_position().row == call_node.end_position().row;
                let call_text: String = if same_line {
                    let row = call_node.start_position().row;
                    if row < lines.len() {
                        lines[row].trim().to_string()
                    } else {
                        matched_target.clone()
                    }
                } else {
                    matched_target.clone()
                };

                // Walk up the tree to find the enclosing function
                let (calling_function, caller_range) =
                    find_enclosing_function(cap.node, &lines, lang);

                callers.push((
                    matched_target,
                    CallerMatch {
                        path: path.to_path_buf(),
                        line,
                        calling_function,
                        call_text,
                        caller_range,
                        content: Arc::clone(&shared_content),
                    },
                ));
            }
        }

        callers
    }) else {
        return Vec::new();
    };

    callers
}

/// Type-like node kinds that can enclose a function definition.
const TYPE_KINDS: &[&str] = &[
    "class_declaration",
    "class_definition",
    "struct_item",
    "impl_item",
    "interface_declaration",
    "trait_item",
    "trait_declaration",
    "type_declaration",
    "enum_item",
    "enum_declaration",
    "module",
    "mod_item",
    "namespace_definition",
];

/// Walk up the AST from `node` to the nearest definition, qualified with its
/// enclosing type/module if one wraps it. Returns the AST node so the caller
/// can read its kind, plus the rendered name and line range.
fn walk_to_enclosing_definition<'a>(
    node: tree_sitter::Node<'a>,
    lines: &[&str],
    lang: crate::types::Lang,
) -> Option<(tree_sitter::Node<'a>, String, (u32, u32))> {
    let mut current = Some(node);
    while let Some(n) = current {
        let def_name = if DEFINITION_KINDS.contains(&n.kind()) {
            extract_definition_name(n, lines)
        } else if lang == crate::types::Lang::Elixir && is_elixir_definition(n, lines) {
            extract_elixir_definition_name(n, lines)
        } else {
            None
        };

        if let Some(name) = def_name {
            let range = (
                n.start_position().row as u32 + 1,
                n.end_position().row as u32 + 1,
            );

            // Walk further up to find an enclosing type/module and qualify the name.
            // `defmodule` is a `call` node, not in TYPE_KINDS, so Elixir needs a
            // separate check to produce `Module.func`.
            let mut parent = n.parent();
            while let Some(p) = parent {
                if TYPE_KINDS.contains(&p.kind()) {
                    if let Some(type_name) = extract_definition_name(p, lines) {
                        return Some((n, format!("{type_name}.{name}"), range));
                    }
                }
                if lang == crate::types::Lang::Elixir && is_elixir_definition(p, lines) {
                    if let Some(type_name) = extract_elixir_definition_name(p, lines) {
                        return Some((n, format!("{type_name}.{name}"), range));
                    }
                }
                parent = p.parent();
            }

            return Some((n, name, range));
        }
        current = n.parent();
    }
    None
}

/// Walk up the AST from a node to find the enclosing function definition.
/// Returns (`function_name`, `line_range`). Top-level renders as `"<top-level>"`.
fn find_enclosing_function(
    node: tree_sitter::Node,
    lines: &[&str],
    lang: crate::types::Lang,
) -> (String, Option<(u32, u32)>) {
    match walk_to_enclosing_definition(node, lines, lang) {
        Some((_, name, range)) => (name, Some(range)),
        None => ("<top-level>".to_string(), None),
    }
}

/// Resolved enclosing-definition context for a (file, line). Used by the
/// search formatter to annotate usages with their containing scope.
#[derive(Debug)]
pub struct EnclosingScope {
    /// Normalized kind label (e.g. `"function"`, `"class"`, `"struct"`).
    pub kind: &'static str,
    /// Identifier of the definition. Qualified with its enclosing type or
    /// module when one wraps it (e.g. `"Class.method"`, `"Module.func"`).
    pub name: String,
}

/// Find the nearest enclosing definition for `(path, line)` by re-parsing
/// the file with tree-sitter (cached on `OutlineCache`). AST-correct across
/// every language tilth supports — replaces parsing the rendered outline
/// string back into structured data.
///
/// Returns `None` if the file isn't a code file, the parse fails, or `line`
/// sits at the top level outside any definition.
pub fn enclosing_definition_at(
    path: &Path,
    line: u32,
    cache: &OutlineCache,
) -> Option<EnclosingScope> {
    if line == 0 {
        return None;
    }
    let parsed = cache.get_or_parse(path)?;
    let lines: Vec<&str> = parsed.content.lines().collect();
    let row = (line - 1) as usize;
    if row >= lines.len() {
        return None;
    }

    let point = tree_sitter::Point { row, column: 0 };
    let target = parsed
        .tree
        .root_node()
        .descendant_for_point_range(point, point)?;

    let (def_node, name, _range) = walk_to_enclosing_definition(target, &lines, parsed.lang)?;
    Some(EnclosingScope {
        kind: kind_label(def_node, &lines, parsed.lang),
        name,
    })
}

/// Map a tree-sitter definition node to a short user-facing label. Every kind
/// we handle is enumerated here, so adding a new language grammar is "add the
/// node kind to this match" with no string heuristics elsewhere.
fn kind_label(node: tree_sitter::Node, lines: &[&str], lang: crate::types::Lang) -> &'static str {
    match node.kind() {
        "function_declaration"
        | "function_definition"
        | "function_item"
        | "method_definition"
        | "method_declaration"
        | "decorated_definition" => "function",
        "class_declaration" | "class_definition" => "class",
        "struct_item" => "struct",
        "interface_declaration" => "interface",
        "trait_declaration" | "trait_item" => "trait",
        "type_alias_declaration" | "type_item" | "type_declaration" => "type",
        "enum_item" | "enum_declaration" => "enum",
        "lexical_declaration" | "variable_declaration" => "variable",
        "const_item" | "const_declaration" => "const",
        "static_item" => "static",
        "property_declaration" => "property",
        "mod_item" | "namespace_definition" => "module",
        "object_declaration" => "object",
        "impl_item" => "impl",
        "export_statement" => "export",
        "call" if lang == crate::types::Lang::Elixir => elixir_kind_label(node, lines),
        _ => "definition",
    }
}

/// Elixir definitions are all `call` nodes; the keyword (`def`, `defmodule`,
/// …) lives in the call's `target` field. Map it to the same vocabulary
/// `kind_label` produces for other languages.
fn elixir_kind_label(node: tree_sitter::Node, lines: &[&str]) -> &'static str {
    let Some(target) = node.child_by_field_name("target") else {
        return "definition";
    };
    match node_text_simple(target, lines).as_str() {
        "defmodule" => "module",
        "defprotocol" => "protocol",
        "defimpl" => "impl",
        "def" | "defp" | "defmacro" | "defmacrop" | "defguard" | "defguardp" | "defdelegate" => {
            "function"
        }
        "defstruct" | "defexception" => "struct",
        _ => "definition",
    }
}

/// Format and rank caller search results with optional expand.
pub fn search_callers_expanded(
    target: &str,
    scope: &Path,
    _cache: &OutlineCache,
    _session: &Session,
    bloom: &crate::index::bloom::BloomFilterCache,
    expand: usize,
    context: Option<&Path>,
    glob: Option<&str>,
) -> Result<String, TilthError> {
    let (callers, target_seen) = find_callers(target, scope, bloom, glob)?;

    if callers.is_empty() {
        return Ok(no_callers_message(target, scope, target_seen, glob));
    }

    // Sort by relevance (context file first, then by proximity)
    let mut sorted_callers = callers;
    rank_callers(&mut sorted_callers, scope, context);

    let total = sorted_callers.len();

    // Collect unique caller names BEFORE truncation for accurate fan-out threshold
    let all_caller_names: HashSet<String> = sorted_callers
        .iter()
        .filter(|c| c.calling_function != "<top-level>")
        .map(|c| c.calling_function.clone())
        .collect();

    sorted_callers.truncate(MAX_MATCHES);

    // Format the output
    let mut output = format!(
        "# Callers of \"{}\" in {} — {} call site{}\n",
        target,
        scope.display(),
        total,
        if total == 1 { "" } else { "s" }
    );

    for (i, caller) in sorted_callers.iter().enumerate() {
        // Header: file:line [caller: calling_function]
        let _ = write!(
            output,
            "\n## {}:{} [caller: {}]\n",
            caller
                .path
                .strip_prefix(scope)
                .unwrap_or(&caller.path)
                .display(),
            caller.line,
            caller.calling_function
        );

        // Show the call text
        let _ = writeln!(output, "→ {}", caller.call_text);

        // Expand if requested and we have the range
        if i < expand {
            if let Some((start, end)) = caller.caller_range {
                // Use cached content — no re-read needed
                let lines: Vec<&str> = caller.content.lines().collect();
                let start_idx = (start as usize).saturating_sub(1);
                let end_idx = (end as usize).min(lines.len());

                output.push('\n');
                output.push_str("```\n");

                for (idx, line) in lines[start_idx..end_idx].iter().enumerate() {
                    let line_num = start_idx + idx + 1;
                    let prefix = if line_num == caller.line as usize {
                        "► "
                    } else {
                        "  "
                    };
                    let _ = writeln!(output, "{prefix}{line_num:4} │ {line}");
                }

                output.push_str("```\n");
            }
        }
    }

    // ── Adaptive 2nd-hop impact analysis ──
    // Use all_caller_names (pre-truncation) for the fan-out threshold check,
    // but search for callers of the full set to capture transitive impact.
    if !all_caller_names.is_empty() && all_caller_names.len() <= IMPACT_FANOUT_THRESHOLD {
        if let Ok(hop2) = find_callers_batch(&all_caller_names, scope, bloom, glob) {
            // Filter out hop-1 matches (same file+line = same call site)
            let hop1_locations: HashSet<(PathBuf, u32)> = sorted_callers
                .iter()
                .map(|c| (c.path.clone(), c.line))
                .collect();

            let hop2_filtered: Vec<_> = hop2
                .into_iter()
                .filter(|(_, m)| !hop1_locations.contains(&(m.path.clone(), m.line)))
                .collect();

            if !hop2_filtered.is_empty() {
                output.push_str("\n── impact (2nd hop) ──\n");

                let mut seen: HashSet<(String, PathBuf)> = HashSet::new();
                let mut count = 0;
                for (via, m) in &hop2_filtered {
                    let key = (m.calling_function.clone(), m.path.clone());
                    if !seen.insert(key) {
                        continue;
                    }
                    if count >= IMPACT_MAX_RESULTS {
                        break;
                    }

                    let rel_path = m.path.strip_prefix(scope).unwrap_or(&m.path).display();
                    let _ = writeln!(
                        output,
                        "  {:<20} {}:{}  \u{2192} {}",
                        m.calling_function, rel_path, m.line, via
                    );
                    count += 1;
                }

                let unique_total = hop2_filtered
                    .iter()
                    .map(|(_, m)| (&m.calling_function, &m.path))
                    .collect::<HashSet<_>>()
                    .len();
                if unique_total > IMPACT_MAX_RESULTS {
                    let _ = writeln!(
                        output,
                        "  ... and {} more",
                        unique_total - IMPACT_MAX_RESULTS
                    );
                }

                let _ = writeln!(
                    output,
                    "\n{} functions affected across 2 hops.",
                    sorted_callers.len() + count
                );
            }
        }
    }

    let tokens = crate::types::estimate_tokens(output.len() as u64);
    let token_str = if tokens >= 1000 {
        format!("~{}.{}k", tokens / 1000, (tokens % 1000) / 100)
    } else {
        format!("~{tokens}")
    };
    let _ = write!(output, "\n\n({token_str} tokens)");
    Ok(output)
}

/// Build the user-facing message when callers search returns no hits.
/// Splits two cases that mean very different things to an agent:
/// `target_seen = true` means the symbol exists somewhere but has no direct
/// call sites — probable indirect dispatch, so we show a richer hint
/// listing the common indirection mechanisms. `target_seen = false` means
/// the literal name never appears in scope — most often a typo or wrong
/// scope, so we suppress the indirect-dispatch hint to avoid misleading
/// the agent.
fn no_callers_message(target: &str, scope: &Path, target_seen: bool, glob: Option<&str>) -> String {
    if !target_seen {
        return format!(
            "# Callers of \"{target}\" in {scope_disp} — no call sites found\n\n\
             The name \"{target}\" does not appear anywhere in scope. \
             Check the spelling, or widen scope if you expected hits outside this directory.",
            scope_disp = scope.display()
        );
    }
    // Only mention glob-driven test exclusion when a glob was actually used.
    // Otherwise the line implies a filter that the caller didn't apply, which
    // would mislead an agent reasoning about what tilth searched.
    let glob_hint = if glob.is_some() {
        "\n  • test files (if `glob` excluded them)"
    } else {
        ""
    };
    format!(
        "# Callers of \"{target}\" in {scope_disp} — no direct call sites found\n\n\
         \"{target}\" appears in the codebase but has no syntactic call sites. \
         tilth detects only direct, by-name calls; this symbol may still be reachable via:\n\
         \n  • interface / trait dispatch (Rust `dyn Trait`, Go interface, Java/Kotlin abstract method)\
         \n  • reflection or dynamic dispatch (`getattr`, `Method::invoke`, `eval`)\
         \n  • framework registration (HTTP routes, JSON-RPC, plugin systems, decorators)\
         \n  • function values stored in maps, structs, or passed as callbacks{glob_hint}\n\
         \nVerify with `tilth_search \"{target}\"` to see how it's referenced before assuming dead code.",
        scope_disp = scope.display()
    )
}

/// Simple ranking: context file first, then by path length (proximity heuristic).
fn rank_callers(callers: &mut [CallerMatch], scope: &Path, context: Option<&Path>) {
    callers.sort_by(|a, b| {
        // Context file wins
        if let Some(ctx) = context {
            match (a.path == ctx, b.path == ctx) {
                (true, false) => return std::cmp::Ordering::Less,
                (false, true) => return std::cmp::Ordering::Greater,
                _ => {}
            }
        }

        // Shorter paths (more similar to scope) rank higher
        let a_rel = a.path.strip_prefix(scope).unwrap_or(&a.path);
        let b_rel = b.path.strip_prefix(scope).unwrap_or(&b.path);
        a_rel
            .components()
            .count()
            .cmp(&b_rel.components().count())
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::OutlineCache;

    fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn no_callers_message_for_unseen_symbol_says_typo_or_scope() {
        let msg = no_callers_message("doesNotExist", Path::new("/repo"), false, None);
        assert!(msg.contains("does not appear anywhere in scope"));
        assert!(msg.contains("Check the spelling"));
        // Must NOT include the indirect-dispatch hint — that would mislead.
        assert!(!msg.contains("interface"));
        assert!(!msg.contains("reflection"));
    }

    #[test]
    fn no_callers_message_for_seen_symbol_lists_indirection_modes() {
        let msg = no_callers_message("Foo", Path::new("/repo"), true, None);
        assert!(msg.contains("appears in the codebase"));
        assert!(msg.contains("interface"));
        assert!(msg.contains("reflection"));
        assert!(msg.contains("framework registration"));
        assert!(msg.contains("Verify with `tilth_search"));
        // Must NOT pretend the symbol is missing — different signal than typo case.
        assert!(!msg.contains("does not appear"));
    }

    /// The "test files (if glob excluded them)" hint is only meaningful when
    /// the caller actually used a glob. Without a glob it would mislead an
    /// agent into thinking tilth filtered something it did not.
    #[test]
    fn no_callers_message_omits_glob_hint_when_no_glob() {
        let msg = no_callers_message("Foo", Path::new("/repo"), true, None);
        assert!(
            !msg.contains("test files"),
            "glob-driven hint must not appear when glob is None: {msg}"
        );
    }

    #[test]
    fn no_callers_message_includes_glob_hint_when_glob_set() {
        let msg = no_callers_message("Foo", Path::new("/repo"), true, Some("*.rs"));
        assert!(
            msg.contains("test files"),
            "glob-driven hint should appear when glob is Some: {msg}"
        );
    }

    #[test]
    fn enclosing_at_rust_top_level_function() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "a.rs", "fn foo() {\n    let x = 1;\n}\n");
        let cache = OutlineCache::new();
        let scope = enclosing_definition_at(&p, 2, &cache).unwrap();
        assert_eq!(scope.kind, "function");
        assert_eq!(scope.name, "foo");
    }

    #[test]
    fn enclosing_at_rust_method_inside_mod() {
        // mod_item has a name field; impl_item does not, so the qualifier path
        // exercised here is mod-name → method-name.
        let tmp = tempfile::tempdir().unwrap();
        let p = write(
            tmp.path(),
            "a.rs",
            "mod outer {\n    fn helper() {\n        let x = 1;\n    }\n}\n",
        );
        let cache = OutlineCache::new();
        let scope = enclosing_definition_at(&p, 3, &cache).unwrap();
        assert_eq!(scope.kind, "function");
        assert_eq!(scope.name, "outer.helper");
    }

    #[test]
    fn enclosing_at_typescript_method_qualifies_with_class() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(
            tmp.path(),
            "a.ts",
            "class Foo {\n  bar() {\n    const x = 1;\n  }\n}\n",
        );
        let cache = OutlineCache::new();
        let scope = enclosing_definition_at(&p, 3, &cache).unwrap();
        assert_eq!(scope.kind, "function");
        assert_eq!(scope.name, "Foo.bar");
    }

    #[test]
    fn enclosing_at_python_method_qualifies_with_class() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(
            tmp.path(),
            "a.py",
            "class Foo:\n    def bar(self):\n        x = 1\n",
        );
        let cache = OutlineCache::new();
        let scope = enclosing_definition_at(&p, 3, &cache).unwrap();
        assert_eq!(scope.kind, "function");
        assert_eq!(scope.name, "Foo.bar");
    }

    #[test]
    fn enclosing_at_elixir_def_qualifies_with_module() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(
            tmp.path(),
            "a.ex",
            "defmodule Foo do\n  def bar do\n    :ok\n  end\nend\n",
        );
        let cache = OutlineCache::new();
        let scope = enclosing_definition_at(&p, 3, &cache).unwrap();
        assert_eq!(scope.kind, "function");
        assert_eq!(scope.name, "Foo.bar");
    }

    #[test]
    fn enclosing_at_elixir_defmodule_kind_is_module() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(
            tmp.path(),
            "a.ex",
            "defmodule Foo do\n  @moduledoc \"hi\"\nend\n",
        );
        let cache = OutlineCache::new();
        let scope = enclosing_definition_at(&p, 2, &cache).unwrap();
        assert_eq!(scope.kind, "module");
        assert_eq!(scope.name, "Foo");
    }

    #[test]
    fn enclosing_at_top_level_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "a.rs", "// just a comment\nfn foo() {}\n");
        let cache = OutlineCache::new();
        assert!(enclosing_definition_at(&p, 1, &cache).is_none());
    }

    #[test]
    fn enclosing_at_zero_line_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "a.rs", "fn foo() {}\n");
        let cache = OutlineCache::new();
        assert!(enclosing_definition_at(&p, 0, &cache).is_none());
    }

    #[test]
    fn enclosing_at_non_code_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "a.md", "# heading\n\nsome text\n");
        let cache = OutlineCache::new();
        assert!(enclosing_definition_at(&p, 3, &cache).is_none());
    }

    #[test]
    fn enclosing_at_caches_parse_across_calls() {
        // Two calls into the same file should reuse the cached parse —
        // observable indirectly by mutating the file between calls without
        // touching mtime: the first parse wins, the second sees stale data
        // because the mtime didn't change. (Test only asserts the cache hit
        // path returns the first-parse result.)
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "a.rs", "fn foo() { let x = 1; }\n");
        let cache = OutlineCache::new();
        let a = enclosing_definition_at(&p, 1, &cache).unwrap();
        let b = enclosing_definition_at(&p, 1, &cache).unwrap();
        assert_eq!(a.name, b.name);
        assert_eq!(a.kind, b.kind);
    }

    #[test]
    fn enclosing_at_kind_labels_for_common_definition_kinds() {
        // One case per kind_label match arm beyond `function`/`module`,
        // so a regression that miscategorizes (e.g.) a Rust `struct` as
        // `definition` would surface here.
        let cases: &[(&str, &str, u32, &str, &str)] = &[
            ("a.rs", "struct Foo { x: u32 }\n", 1, "struct", "Foo"),
            ("b.rs", "enum Color { Red, Blue }\n", 1, "enum", "Color"),
            (
                "c.rs",
                "trait Greeter { fn hi(&self); }\n",
                1,
                "trait",
                "Greeter",
            ),
            (
                "d.ts",
                "interface Shape { area(): number; }\n",
                1,
                "interface",
                "Shape",
            ),
            ("e.ts", "class Widget { x = 1; }\n", 1, "class", "Widget"),
        ];
        let cache = OutlineCache::new();
        for (filename, content, line, kind, name) in cases {
            let tmp = tempfile::tempdir().unwrap();
            let p = write(tmp.path(), filename, content);
            let scope = enclosing_definition_at(&p, *line, &cache)
                .unwrap_or_else(|| panic!("no scope returned for {filename}"));
            assert_eq!(scope.kind, *kind, "kind mismatch for {filename}");
            assert_eq!(scope.name, *name, "name mismatch for {filename}");
        }
    }

    #[test]
    fn enclosing_at_rust_impl_block_does_not_qualify_with_type() {
        // tree-sitter-rust's `impl_item` exposes its type via a `type` field,
        // not via the `name`/`identifier`/`declarator` fields that
        // extract_definition_name probes. So methods inside `impl Foo {...}`
        // produce the bare function name, not `"Foo.bar"`. Pre-existing
        // behavior of find_enclosing_function — pinned here so a future
        // qualifier improvement is an intentional, visible change.
        let tmp = tempfile::tempdir().unwrap();
        let p = write(
            tmp.path(),
            "a.rs",
            "struct Foo;\nimpl Foo {\n    fn bar(&self) {\n        let x = 1;\n    }\n}\n",
        );
        let cache = OutlineCache::new();
        let scope = enclosing_definition_at(&p, 4, &cache).unwrap();
        assert_eq!(scope.kind, "function");
        assert_eq!(scope.name, "bar");
    }
}
