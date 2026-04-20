use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

use super::file_metadata;
use crate::lang::treesitter::{
    definition_weight, elixir_definition_weight, extract_definition_name,
    extract_elixir_definition_name, extract_impl_trait, extract_impl_type,
    extract_implemented_interfaces, is_elixir_definition, DEFINITION_KINDS,
};

use crate::error::TilthError;
use crate::lang::detect_file_type;
use crate::lang::outline::outline_language;
use crate::search::rank;
use crate::types::{FileType, Match, SearchResult};
use grep_regex::RegexMatcher;
use grep_searcher::sinks::UTF8;
use grep_searcher::Searcher;

const MAX_MATCHES: usize = 10;
/// Stop walking once we have this many raw definition matches.
const EARLY_QUIT_THRESHOLD_DEFINITIONS: usize = 50;
/// Stop walking once we have this many raw usage matches.
const EARLY_QUIT_THRESHOLD_USAGES: usize = MAX_MATCHES * 3;

/// Symbol search: find definitions via tree-sitter, usages via ripgrep, concurrently.
/// Merge results, deduplicate, definitions first.
pub fn search(
    query: &str,
    scope: &Path,
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
        || find_definitions(query, scope, glob),
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
    merged.truncate(MAX_MATCHES);

    Ok(SearchResult {
        query: query.to_string(),
        scope: scope.to_path_buf(),
        matches: merged,
        total_found: total,
        definitions: def_count,
        usages: usage_count,
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

        Box::new(move |entry| {
            // Early termination: enough definitions found
            if found_count.load(Ordering::Relaxed) >= EARLY_QUIT_THRESHOLD_DEFINITIONS {
                return ignore::WalkState::Quit;
            }

            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };

            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }

            let path = entry.path();

            // Skip oversized files — avoid tree-sitter parsing multi-MB minified bundles
            if let Ok(meta) = std::fs::metadata(path) {
                if meta.len() > 500_000 {
                    return ignore::WalkState::Continue;
                }
            }

            // Single read: read file once, use buffer for both check and parse
            let Ok(content) = fs::read_to_string(path) else {
                return ignore::WalkState::Continue;
            };

            // Fast byte check via memchr::memmem (SIMD) — skip files without the symbol
            if memchr::memmem::find(content.as_bytes(), needle).is_none() {
                return ignore::WalkState::Continue;
            }

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
                find_defs_treesitter(path, query, ts_lang, lang, &content, file_lines, mtime)
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
    lang: Option<crate::types::Lang>,
    content: &str,
    file_lines: u32,
    mtime: SystemTime,
) -> Vec<Match> {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(ts_lang).is_err() {
        return Vec::new();
    }

    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };

    let lines: Vec<&str> = content.lines().collect();
    let root = tree.root_node();
    let mut defs = Vec::new();

    walk_for_definitions(
        root, query, path, &lines, file_lines, mtime, &mut defs, lang, 0,
    );

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
    lang: Option<crate::types::Lang>,
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
    } else if lang == Some(crate::types::Lang::Elixir) && is_elixir_definition(node, lines) {
        // Elixir: definitions are `call` nodes — check separately
        if let Some(name) = extract_elixir_definition_name(node, lines) {
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
                    def_weight: elixir_definition_weight(node, lines),
                    impl_target: None,
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
            lang,
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
            // Early termination: enough usages found
            if found_count.load(Ordering::Relaxed) >= EARLY_QUIT_THRESHOLD_USAGES {
                return ignore::WalkState::Quit;
            }

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
            Some(crate::types::Lang::Rust),
            code,
            15,
            SystemTime::now(),
        );
        assert!(!defs.is_empty(), "should find 'hello' definition");
        assert!(defs[0].is_definition);
        assert!(defs[0].def_range.is_some());

        let defs = find_defs_treesitter(
            std::path::Path::new("test.rs"),
            "Foo",
            &ts_lang,
            Some(crate::types::Lang::Rust),
            code,
            15,
            SystemTime::now(),
        );
        assert!(!defs.is_empty(), "should find 'Foo' definition");

        let defs = find_defs_treesitter(
            std::path::Path::new("test.rs"),
            "dispatch_tool",
            &ts_lang,
            Some(crate::types::Lang::Rust),
            code,
            15,
            SystemTime::now(),
        );
        assert!(!defs.is_empty(), "should find 'dispatch_tool' definition");
    }

    /// Helper: search for an Elixir definition by name in a code snippet.
    fn elixir_find(code: &str, name: &str) -> Vec<Match> {
        let ts_lang = crate::lang::outline::outline_language(crate::types::Lang::Elixir).unwrap();
        let lines = code.lines().count() as u32;
        find_defs_treesitter(
            std::path::Path::new("test.ex"),
            name,
            &ts_lang,
            Some(crate::types::Lang::Elixir),
            code,
            lines,
            SystemTime::now(),
        )
    }

    #[test]
    fn elixir_definitions_detected() {
        let code = r#"defmodule MyApp.Greeter do
  @type t :: %{name: String.t()}

  def hello(name) do
    "Hello, #{name}!"
  end

  defp private_helper(x), do: x + 1

  defmacro my_macro(expr) do
    quote do: unquote(expr)
  end
end
"#;
        // Dotted module name
        let defs = elixir_find(code, "MyApp.Greeter");
        assert!(!defs.is_empty(), "should find 'MyApp.Greeter' module def");
        assert!(defs[0].is_definition);

        // Public function (block form with parens)
        assert!(
            !elixir_find(code, "hello").is_empty(),
            "should find 'hello'"
        );

        // Private function (keyword form: `, do:`)
        assert!(
            !elixir_find(code, "private_helper").is_empty(),
            "should find 'private_helper'"
        );

        // Macro
        assert!(
            !elixir_find(code, "my_macro").is_empty(),
            "should find 'my_macro'"
        );
    }

    #[test]
    fn elixir_guard_clause_definitions() {
        let code = r#"defmodule Guards do
  def safe_div(a, b) when b != 0 do
    a / b
  end

  defp checked(x) when is_integer(x), do: x

  defguard is_positive(x) when x > 0
end
"#;
        // Guard clause with `when` — block form
        assert!(
            !elixir_find(code, "safe_div").is_empty(),
            "should find 'safe_div' with guard clause"
        );

        // Guard clause with `when` — keyword form
        assert!(
            !elixir_find(code, "checked").is_empty(),
            "should find 'checked' with guard clause"
        );

        // defguard
        assert!(
            !elixir_find(code, "is_positive").is_empty(),
            "should find 'is_positive' defguard"
        );
    }

    #[test]
    fn elixir_multi_clause_and_no_arg() {
        let code = r#"defmodule Dispatch do
  def handle(:ok), do: :success
  def handle(:error), do: :failure

  def version, do: "1.0"
end
"#;
        // Multi-clause: both clauses should be found
        let defs = elixir_find(code, "handle");
        assert!(
            defs.len() >= 2,
            "should find both 'handle' clauses, got {}: {defs:?}",
            defs.len()
        );

        // No-arg function (bare identifier, no parens)
        assert!(
            !elixir_find(code, "version").is_empty(),
            "should find no-arg 'version'"
        );
    }

    #[test]
    fn elixir_protocol_impl_exception() {
        let code = r#"defprotocol Printable do
  @callback format(t) :: String.t()
  def to_string(data)
end

defimpl Printable, for: User do
  def to_string(user), do: user.name
end

defmodule MyError do
  defexception [:message, :code]
end
"#;
        // Protocol + defimpl: both indexed under the protocol name "Printable"
        let defs = elixir_find(code, "Printable");
        assert!(
            defs.len() >= 2,
            "should find both defprotocol and defimpl for 'Printable', got {}",
            defs.len()
        );

        // defexception
        assert!(
            !elixir_find(code, "defexception").is_empty(),
            "should find 'defexception'"
        );

        // Module containing exception
        assert!(
            !elixir_find(code, "MyError").is_empty(),
            "should find 'MyError' module"
        );
    }

    #[test]
    fn elixir_delegate_and_nested_modules() {
        let code = r#"defmodule Outer do
  defdelegate count(list), to: Enum

  defmodule Inner do
    def nested_func, do: :ok
  end
end
"#;
        // defdelegate
        assert!(
            !elixir_find(code, "count").is_empty(),
            "should find 'count' defdelegate"
        );

        // Nested module
        assert!(
            !elixir_find(code, "Inner").is_empty(),
            "should find nested 'Inner' module"
        );
    }
}
