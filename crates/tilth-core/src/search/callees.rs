use std::collections::HashSet;
use std::path::{Path, PathBuf};

use streaming_iterator::StreamingIterator;

use crate::lang::outline::{get_outline_entries, outline_language};
use crate::lang::treesitter::compiled_query;
use crate::types::{Lang, OutlineEntry};

/// A resolved callee: a function/method called from within an expanded definition.
#[derive(Debug, Clone)]
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
    /// 2nd-hop callees resolved from within this callee's body, as many as
    /// the shared budget had left.
    pub children: Vec<ResolvedCallee>,
    /// How many 2nd-hop callees this callee has. More than `children.len()`
    /// when the budget cut some.
    pub children_found: usize,
}

/// Extract names of functions/methods called within a given line range.
/// Uses tree-sitter query patterns to find call expressions.
///
/// If `def_range` is `Some((start, end))`, only callees whose match position
/// falls within lines `start..=end` (1-indexed) are returned.
/// Returns a deduplicated, sorted list of callee names.
#[must_use]
pub fn extract_callee_names(
    content: &str,
    lang: Lang,
    def_range: Option<(u32, u32)>,
) -> Vec<String> {
    let Some(ts_lang) = outline_language(lang) else {
        return Vec::new();
    };

    let Some(query_str) = super::callee_query::callee_query_str(lang) else {
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

    let callee_filter = crate::lang::spec::spec(lang).callee_filter;

    let Some(query) = compiled_query(&ts_lang, query_str) else {
        return Vec::new();
    };
    let Some(callee_idx) = query.capture_index_for_name("callee") else {
        return Vec::new();
    };

    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), content_bytes);
    let mut names: Vec<String> = Vec::new();

    while let Some(m) = matches.next() {
        for cap in m.captures() {
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

            // Some patterns match more than a call (see `callee_filter`).
            if callee_filter.is_some_and(|keep| !keep(&cap.node)) {
                continue;
            }

            if let Ok(text) = cap.node.utf8_text(content_bytes) {
                names.push(text.to_string());
            }
        }
    }

    names.sort();
    names.dedup();

    // Elixir: the callee query `(call target: (identifier) @callee)` also captures
    // definition keywords (def, defmodule, etc.) and import keywords (use, import,
    // alias, require) since those are all `call` nodes. Filter them out.
    if lang == Lang::Elixir {
        names.retain(|n| !is_elixir_keyword(n));
    }

    names
}

/// Keywords that should not appear as callee names in Elixir.
/// These are definition and import forms that are syntactically `call` nodes.
/// Superset of `ELIXIR_DEFINITION_TARGETS` (elixir.rs) plus import keywords
/// (`use`, `import`, `alias`, `require`) and `defoverridable`.
fn is_elixir_keyword(name: &str) -> bool {
    matches!(
        name,
        "def"
            | "defp"
            | "defmodule"
            | "defmacro"
            | "defmacrop"
            | "defguard"
            | "defguardp"
            | "defdelegate"
            | "defstruct"
            | "defexception"
            | "defprotocol"
            | "defimpl"
            | "defoverridable"
            | "use"
            | "import"
            | "alias"
            | "require"
    )
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
    bloom: &crate::index::bloom::BloomFilterCache,
) -> Vec<ResolvedCallee> {
    if callee_names.is_empty() {
        return Vec::new();
    }

    let file_type = crate::lang::detect_file_type_with_content(source_path, source_content);
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
    let imported = crate::imports::resolve_related_files_with_content(source_path, source_content);

    for import_path in imported {
        if remaining.is_empty() {
            break;
        }

        // Read + bloom prefilter via shared helper. Skip the file when no
        // remaining symbol is bloom-positive.
        let Some((import_content, _mtime)) = super::bloom_walk::read_with_bloom_check(
            &import_path,
            remaining.iter().copied(),
            bloom,
            crate::source::MAX_FILE_SIZE,
        ) else {
            continue;
        };

        let import_type = crate::lang::detect_file_type_with_content(&import_path, &import_content);
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
    // A tighter bound than `source::MAX_FILE_SIZE`: this scan parses up to
    // `MAX_FILES` siblings on every expansion, not one file a query names.
    const MAX_SIBLING_SIZE: u64 = 100_000;

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
                && e.metadata().is_ok_and(|m| m.len() <= MAX_SIBLING_SIZE)
        })
        .map(|e| e.path())
        .collect();

    go_files.sort();
    go_files.truncate(MAX_FILES);

    for go_path in go_files {
        if remaining.is_empty() {
            break;
        }

        let Ok(content) = crate::source::read_source(&go_path) else {
            continue;
        };

        let outline = get_outline_entries(&content, Lang::Go);
        resolve_from_entries(&outline, &go_path, remaining, resolved);
    }
}

/// The 2nd hop under each of `parents`: the callees its body calls.
///
/// `parents` are the callees the caller shows, in the order it shows them,
/// and `first_hop` is every callee the definition calls. `budget` caps the
/// 2nd-hop callees kept across all parents, in that order. Each parent still
/// resolves all of its own, so its node counts what the budget cut. A 2nd-hop
/// callee is left out when it is in `first_hop` or kept under an earlier
/// parent, which also stops cycles.
#[must_use]
pub fn resolve_second_hops(
    parents: Vec<ResolvedCallee>,
    first_hop: &[ResolvedCallee],
    bloom: &crate::index::bloom::BloomFilterCache,
    budget: usize,
) -> Vec<ResolvedCalleeNode> {
    let mut visited: HashSet<(PathBuf, u32)> = first_hop
        .iter()
        .map(|c| (c.file.clone(), c.start_line))
        .collect();
    let mut budget_remaining = budget;
    let mut nodes = Vec::with_capacity(parents.len());

    for parent in parents {
        let mut children = resolve_second_hop(&parent, bloom, &visited);
        let children_found = children.len();
        children.truncate(budget_remaining);
        budget_remaining -= children.len();
        visited.extend(children.iter().map(|c| (c.file.clone(), c.start_line)));
        nodes.push(ResolvedCalleeNode {
            callee: parent,
            children,
            children_found,
        });
    }

    nodes
}

/// Resolve 2nd-hop callees for a single parent callee, leaving out the
/// parent itself and every callee already `visited`.
fn resolve_second_hop(
    parent: &ResolvedCallee,
    bloom: &crate::index::bloom::BloomFilterCache,
    visited: &HashSet<(PathBuf, u32)>,
) -> Vec<ResolvedCallee> {
    let Ok(content) = crate::source::read_source(&parent.file) else {
        return Vec::new();
    };
    let file_type = crate::lang::detect_file_type_with_content(&parent.file, &content);
    let crate::types::FileType::Code(lang) = file_type else {
        return Vec::new();
    };

    let def_range = Some((parent.start_line, parent.end_line));
    let nested_names = extract_callee_names(&content, lang, def_range);

    if nested_names.is_empty() {
        return Vec::new();
    }

    let mut resolved = resolve_callees(&nested_names, &parent.file, &content, bloom);

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

    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_kotlin_callee_names() {
        let kotlin = r#"fun example() {
    println("hello")
    val x = listOf(1, 2, 3)
    x.forEach { it.toString() }
}
"#;
        let names = extract_callee_names(kotlin, crate::types::Lang::Kotlin, None);

        assert!(
            names.contains(&"println".to_string()),
            "expected println, got: {names:?}"
        );
        assert!(
            names.contains(&"listOf".to_string()),
            "expected listOf, got: {names:?}"
        );
        assert!(
            names.contains(&"forEach".to_string()),
            "expected forEach, got: {names:?}"
        );
        assert!(
            names.contains(&"toString".to_string()),
            "expected toString, got: {names:?}"
        );
    }

    /// tree-sitter parses a macro invocation's arguments as a `token_tree` of
    /// raw tokens, so `alpha` has no `call_expression` node. The three names
    /// that must NOT appear pin the two halves that keep the pattern honest:
    /// `beta` needs `callee_filter` (only an anonymous comma stands between it
    /// and the tuple), `idx` and `Lit` need the `"("` child, their trees being
    /// `[..]` and `{..}`.
    #[test]
    fn extract_rust_callee_names_inside_macro_arguments() {
        let rust = r#"fn uses(v: u32, out: &mut String) {
    let _ = write!(out, "{}", alpha(1));
    assert_eq!(beta, (1, 2));
    println!("{:?}", idx[1]);
    let _ = vec![Lit { a: 1 }];
    gamma(v);
}
"#;
        let names = extract_callee_names(rust, Lang::Rust, None);

        assert!(
            names.contains(&"alpha".to_string()),
            "expected alpha called inside a macro argument, got: {names:?}"
        );
        assert!(
            names.contains(&"gamma".to_string()),
            "plain calls must still be found, got: {names:?}"
        );
        assert!(
            !names.contains(&"beta".to_string()),
            "a bare token before a tuple is not a call, got: {names:?}"
        );
        assert!(
            !names.contains(&"idx".to_string()),
            "indexing is not a call, got: {names:?}"
        );
        assert!(
            !names.contains(&"Lit".to_string()),
            "a braced literal is not a call, got: {names:?}"
        );
    }

    /// An attribute's argument list is a `token_tree` as well, and the
    /// expressions in one do run — thiserror compiles `#[error(..)]` arguments
    /// into `Display`. Reaching them is deliberate; the cost is that `cfg`
    /// predicates, which are not calls, have the same shape.
    #[test]
    fn extract_rust_callee_names_inside_attribute_arguments() {
        let rust = r#"#[derive(Debug)]
#[cfg(not(any(unix, windows)))]
#[error("{}: {reason}", path.display())]
struct Failure;
"#;
        let names = extract_callee_names(rust, Lang::Rust, None);

        assert!(
            names.contains(&"display".to_string()),
            "an expression inside an attribute is a real call, got: {names:?}"
        );
    }

    #[test]
    fn extract_php_callee_names() {
        let php = r"<?php
function run($svc): void {
    local_helper();
    Foo\Bar::staticCall();
    $svc->methodCall();
    $svc?->nullableCall();
}
";

        let names = extract_callee_names(php, Lang::Php, None);

        assert!(names.contains(&"local_helper".to_string()));
        assert!(names.contains(&"staticCall".to_string()));
        assert!(names.contains(&"methodCall".to_string()));
        assert!(names.contains(&"nullableCall".to_string()));
    }

    #[test]
    fn extract_elixir_callee_names() {
        let elixir = r#"defmodule Example do
  def run(conn) do
    result = query(conn, "SELECT 1")
    Enum.map(result, &to_string/1)
    IO.puts("done")
    local_func()
  end
end
"#;
        let names = extract_callee_names(elixir, Lang::Elixir, None);

        assert!(
            names.contains(&"query".to_string()),
            "expected query, got: {names:?}"
        );
        assert!(
            names.contains(&"map".to_string()),
            "expected map (from Enum.map), got: {names:?}"
        );
        assert!(
            names.contains(&"puts".to_string()),
            "expected puts (from IO.puts), got: {names:?}"
        );
        assert!(
            names.contains(&"local_func".to_string()),
            "expected local_func, got: {names:?}"
        );

        // Definition keywords must NOT appear as callees
        assert!(
            !names.contains(&"def".to_string()),
            "definition keyword 'def' should be filtered, got: {names:?}"
        );
        assert!(
            !names.contains(&"defmodule".to_string()),
            "definition keyword 'defmodule' should be filtered, got: {names:?}"
        );
    }

    #[test]
    fn extract_elixir_callee_names_pipes() {
        let elixir = r#"defmodule Pipes do
  def run(conn) do
    conn
    |> prepare("sql")
    |> execute()
    |> Enum.map(&transform/1)
  end
end
"#;
        let names = extract_callee_names(elixir, Lang::Elixir, None);

        // Pipe targets are regular call nodes — the callee query should find them
        assert!(
            names.contains(&"prepare".to_string()),
            "expected prepare from pipe, got: {names:?}"
        );
        assert!(
            names.contains(&"execute".to_string()),
            "expected execute from pipe, got: {names:?}"
        );
        assert!(
            names.contains(&"map".to_string()),
            "expected map from Enum.map pipe, got: {names:?}"
        );
    }
}
