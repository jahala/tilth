use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

use super::file_metadata;
use tilth_core::lang::elixir::is_elixir_definition;
use tilth_core::lang::spec::{spec, DefinitionOps, DEFAULT_DEFS, DEFAULT_DEF_KINDS};
use tilth_core::lang::treesitter::{
    extract_definition_name, extract_impl_trait, extract_impl_type, extract_implemented_interfaces,
    is_or_body_of, node_text_simple, NodeTextMode, NAMESPACE_KINDS, PASS_THROUGH_KINDS,
};

use crate::search::rank;
use crate::types::{Match, SearchResult};
use grep_regex::RegexMatcher;
use grep_searcher::sinks::UTF8;
use grep_searcher::Searcher;
use tilth_core::error::TilthError;
use tilth_core::lang::detect_file_type;
use tilth_core::lang::outline::{heading_text, outline_language, parse_markdown};
use tilth_core::types::FileType;

const MAX_MATCHES: usize = 10;

/// Match-count cap when `--full` is set. Generous but bounded so a `tilth
/// foo --full` on a huge repo can't blow up output.
const FULL_MAX_MATCHES: usize = 100;

/// Display-side stratum: 0 = code def, 1 = doc-heading def, 2 = usage. Used
/// as a stable sort key after `rank::sort` so the `MAX_MATCHES` cap can't drop
/// real code defs in favor of markdown-heading defs of the same query.
fn stratum_for_display(m: &Match) -> u8 {
    if m.is_definition {
        u8::from(m.def_weight < 60)
    } else {
        2
    }
}

/// Put matches in display order: by rank, then stratified so a real code
/// definition always comes before a markdown-heading "definition" of the same
/// query, and both before usages. Stable within each stratum, so the
/// relevance ordering from `rank::sort` is kept. The walk applies this to
/// each file and keeps its first `k`, then to the union with what it kept so
/// far. That loses nothing: the order restricted to one file is the same
/// order, so a match in the overall first `k` is in its own file's first `k`.
fn display_order(
    matches: &mut Vec<Match>,
    query: &str,
    scope: &Path,
    context: Option<&Path>,
    now: SystemTime,
) {
    rank::sort(matches, query, scope, context, now);
    matches.sort_by_key(stratum_for_display);
}

/// Symbol search: find definitions via tree-sitter and usages via ripgrep in
/// one walk over `scope`. The walk always runs to the end, so the totals are
/// exact and the result does not depend on thread timing.
///
/// `full` raises the number of matches returned from `MAX_MATCHES` to
/// `FULL_MAX_MATCHES`. It does not change the totals.
pub fn search(
    query: &str,
    scope: &Path,
    context: Option<&Path>,
    glob: Option<&str>,
    full: bool,
) -> Result<SearchResult, TilthError> {
    let max_matches = if full { FULL_MAX_MATCHES } else { MAX_MATCHES };
    search_capped(query, scope, context, glob, max_matches, max_matches)
}

/// The display cap and the collection bound are the same number in
/// production. They are separate parameters so a test can raise one without
/// the other and show that the bound leaves the result untouched.
fn search_capped(
    query: &str,
    scope: &Path,
    context: Option<&Path>,
    glob: Option<&str>,
    max_matches: usize,
    keep: usize,
) -> Result<SearchResult, TilthError> {
    let word_pattern = format!(r"\b{}\b", regex_syntax::escape(query));
    let matcher = RegexMatcher::new(&word_pattern).map_err(|e| TilthError::InvalidQuery {
        query: query.to_string(),
        reason: e.to_string(),
    })?;

    // One clock for the per-file bound and the final sort, so both rank by
    // the same scores.
    let now = SystemTime::now();
    let walked = walk(query, &matcher, scope, context, glob, now, keep)?;

    let definitions: usize = walked.per_file.iter().map(|f| f.definitions).sum();
    let total: usize = walked.per_file.iter().map(|f| f.total).sum();
    let mut merged = walked.kept;
    display_order(&mut merged, query, scope, context, now);

    let file_facets: Vec<super::facets::FileFacets> =
        walked.per_file.into_iter().map(|f| f.facets).collect();
    let facet_totals = super::facets::totals(&file_facets, merged.iter().find(|m| m.is_definition));

    merged.truncate(max_matches);

    Ok(SearchResult {
        query: query.to_string(),
        scope: scope.to_path_buf(),
        matches: merged,
        total_found: total,
        definitions,
        usages: total - definitions,
        facet_totals,
    })
}

/// What the walk found in one file, beyond the matches it kept.
struct FileCount {
    total: usize,
    definitions: usize,
    facets: super::facets::FileFacets,
}

struct Walked {
    /// The first `keep` matches in display order.
    kept: Vec<Match>,
    per_file: Vec<FileCount>,
}

/// Walk `scope` once and collect the definitions and usages of `query`.
/// Every file is visited and counted, so the totals are exact. Only the
/// first `keep` matches in display order are held: each file's first `keep`
/// are merged into the running first `keep` as the file is read. The display
/// order is total across files, since it ends on path and line, so what is
/// kept does not depend on the order files arrive in.
fn walk(
    query: &str,
    matcher: &RegexMatcher,
    scope: &Path,
    context: Option<&Path>,
    glob: Option<&str>,
    now: SystemTime,
    keep: usize,
) -> Result<Walked, TilthError> {
    let found: Mutex<Walked> = Mutex::new(Walked {
        kept: Vec::new(),
        per_file: Vec::new(),
    });

    let walker = tilth_core::search::walk::walker(scope, glob)?;

    walker.run(|| {
        let found = &found;

        Box::new(move |entry| {
            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };

            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }

            let path = entry.path();
            let mut file_matches = matches_in_file(path, query, matcher);
            if file_matches.is_empty() {
                return ignore::WalkState::Continue;
            }

            let count = FileCount {
                total: file_matches.len(),
                definitions: file_matches.iter().filter(|m| m.is_definition).count(),
                facets: super::facets::count_file(path, &file_matches),
            };
            display_order(&mut file_matches, query, scope, context, now);
            file_matches.truncate(keep);

            let mut found = found
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            found.kept.extend(file_matches);
            display_order(&mut found.kept, query, scope, context, now);
            found.kept.truncate(keep);
            found.per_file.push(count);

            ignore::WalkState::Continue
        })
    });

    Ok(found
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
}

/// The definitions and usages of `query` in one file, with a usage on a
/// definition's line dropped in favour of the definition.
///
/// Single-read design: reads the file once, checks for the symbol via
/// `memchr::memmem` (SIMD), then reuses the buffer for both the tree-sitter
/// definition pass and the word-boundary usage pass. Definitions fall back
/// to a keyword heuristic for code without a grammar.
fn matches_in_file(path: &Path, query: &str, matcher: &RegexMatcher) -> Vec<Match> {
    // Skip files that look minified by filename — `.min.js`, `app-min.css`.
    if path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(tilth_core::lang::detection::is_minified_by_name)
    {
        return Vec::new();
    }

    // Skip oversized files — avoid tree-sitter parsing multi-MB minified bundles
    let file_size = match std::fs::metadata(path) {
        Ok(meta) => {
            if meta.len() > 500_000 {
                return Vec::new();
            }
            meta.len()
        }
        Err(_) => 0,
    };

    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };

    // Fast byte check — a word-boundary usage or a definition both need the
    // symbol's bytes somewhere in the file.
    if memchr::memmem::find(&bytes, query.as_bytes()).is_none() {
        return Vec::new();
    }

    // Catch unmarked minified bundles that slipped past the filename check.
    if file_size >= tilth_core::lang::detection::MINIFIED_CHECK_THRESHOLD
        && tilth_core::lang::detection::is_minified_by_content(&bytes)
    {
        return Vec::new();
    }

    let (file_lines, mtime) = file_metadata(path);

    let mut matches = match std::str::from_utf8(&bytes) {
        Ok(content) => definitions_in(path, query, content, file_lines, mtime),
        Err(_) => Vec::new(),
    };
    let def_count = matches.len();

    let mut searcher = Searcher::new();
    let _ = searcher.search_slice(
        matcher,
        &bytes,
        UTF8(|line_num, line| {
            let line_num = line_num as u32;
            let on_definition = matches[..def_count].iter().any(|d| d.line == line_num);
            if !on_definition {
                matches.push(Match {
                    path: path.to_path_buf(),
                    line: line_num,
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
            }
            Ok(true)
        }),
    );

    matches
}

/// The definitions of `query` in one file's content.
fn definitions_in(
    path: &Path,
    query: &str,
    content: &str,
    file_lines: u32,
    mtime: SystemTime,
) -> Vec<Match> {
    // Try tree-sitter structural detection
    let file_type = detect_file_type(path);
    let lang = match file_type {
        FileType::Code(l) => Some(l),
        _ => None,
    };

    let ts_language = lang.and_then(outline_language);

    if let Some(ref ts_lang) = ts_language {
        return find_defs_treesitter(path, query, ts_lang, lang, content, file_lines, mtime);
    }

    // Per-file-type fallback dispatch. The semantics of "definition"
    // differ by file kind, so handle them separately:
    //
    // * Code without a tree-sitter grammar: keyword heuristic (looks
    //   for lines starting with `function`/`const`/`class`/etc.).
    // * Markdown / RST: heading-as-definition. A heading whose text
    //   contains the query (`## parseCitations` in a doc) marks that
    //   section AS being about the symbol — that is the documentation
    //   analogue of a code definition. Quoted code blocks inside
    //   docs are NOT treated as definitions; they're usages, because
    //   the keyword heuristic would false-positive on every snippet
    //   that quotes the real source. Heading defs carry a lower
    //   `def_weight` (30) than code definitions (60-80) so the real
    //   source still ranks first.
    // * Structured data / tabular / log / other: no fallback.
    //   Mentions are config values, data, or noise — not definitions.
    //   (A future patch could treat top-level config keys matching
    //   the query as soft definitions, but that's ambiguous enough
    //   to skip for now.)
    match file_type {
        FileType::Code(_) => find_defs_heuristic_buf(path, query, content, file_lines, mtime),
        FileType::Markdown => find_defs_markdown_buf(path, query, content, file_lines, mtime),
        _ => Vec::new(),
    }
}

/// Tree-sitter structural definition detection.
/// Accepts pre-read content — no redundant file read.
fn find_defs_treesitter(
    path: &Path,
    query: &str,
    ts_lang: &tree_sitter::Language,
    lang: Option<tilth_core::types::Lang>,
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
    lang: Option<tilth_core::types::Lang>,
    depth: usize,
) {
    if depth > 3 {
        return;
    }

    let kind = node.kind();

    // Definition kinds and name/weight ops come from the per-language spec. For
    // a known language these are `spec(lang).definition_kinds` / `.definitions`
    // (the shared defaults, except where a language such as Elixir or Go
    // carries its own); when `lang` is unknown we fall back to the shared
    // defaults.
    let (def_kinds, def_ops): (&[&str], &DefinitionOps) = match lang {
        Some(l) => {
            let s = spec(l);
            (s.definition_kinds, &s.definitions)
        }
        None => (DEFAULT_DEF_KINDS, &DEFAULT_DEFS),
    };

    if def_kinds.contains(&kind) {
        // Check if this node defines the queried symbol
        if defines_name(node, lines, query, def_ops) {
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
                def_weight: (def_ops.weight)(node, lines),
                impl_target: None,
            });
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
    } else if lang == Some(tilth_core::types::Lang::Elixir) && is_elixir_definition(node, lines) {
        // Elixir: definitions are `call` nodes — check separately. Name and
        // weight come from `spec(Elixir).definitions` via `def_ops`.
        if let Some(name) = (def_ops.extract_name)(node, lines) {
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
                    def_weight: (def_ops.weight)(node, lines),
                    impl_target: None,
                });
            }
        }
    }

    // Recurse into children (for nested definitions, class bodies, impl blocks,
    // etc.). Namespaces and pass-through wrappers (`#ifdef`, `extern "C"`,
    // `template <...>`) add no nesting, so they do not count toward the depth
    // limit: a method in a class in a namespace is found like one in a
    // top-level class.
    let child_depth =
        if is_or_body_of(node, NAMESPACE_KINDS) || is_or_body_of(node, PASS_THROUGH_KINDS) {
            depth
        } else {
            depth + 1
        };
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
            child_depth,
        );
    }
}

/// Whether definition `node` binds the name `query`. Most definitions bind
/// one name, which `extract_name` returns. A Go `const` or `var` spec can bind
/// several (`var a, b int`), each a named child under the `name` field, so
/// the later names are checked too. A node `extract_name` gives no name binds
/// nothing: a C `struct point` without a body names a type, it defines none.
fn defines_name(node: tree_sitter::Node, lines: &[&str], query: &str, ops: &DefinitionOps) -> bool {
    let Some(first) = (ops.extract_name)(node, lines) else {
        return false;
    };
    if first == query {
        return true;
    }
    let mut cursor = node.walk();
    let found = node
        .children_by_field_name("name", &mut cursor)
        .any(|n| n.is_named() && node_text_simple(n, lines, NodeTextMode::Full) == query);
    found
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

/// Markdown heading definition detector.
///
/// An ATX heading (`^#{1,6}\s+<text>`) in a `.md`/`.mdx`/`.rst` file is
/// treated as a soft definition of the section about <query> when <query>
/// appears in <text> as a whole identifier (flanked by non-word chars).
/// Setext headings, indented code blocks, and lines inside fenced code
/// blocks are filtered out by the tree-sitter-md parser before we see them.
///
/// Section span (`def_range`) covers the heading line through the last
/// non-blank line before the next same-or-higher-level heading, and is
/// computed from the enclosing `section` node's end position. Sub-headings
/// nest as child sections of the parent and don't terminate the parent.
///
/// Whole-identifier match (not substring-anywhere) prevents false positives
/// like query `func` matching heading `## refactoring guidelines`.
fn find_defs_markdown_buf(
    path: &Path,
    query: &str,
    content: &str,
    file_lines: u32,
    mtime: SystemTime,
) -> Vec<Match> {
    let Some(tree) = parse_markdown(content) else {
        return Vec::new();
    };
    let lines: Vec<&str> = content.lines().collect();
    let mut defs = Vec::new();
    walk_md_sections(
        tree.root_node(),
        &lines,
        query,
        path,
        file_lines,
        mtime,
        &mut defs,
    );
    defs
}

#[allow(clippy::too_many_arguments)]
fn walk_md_sections(
    node: tree_sitter::Node,
    lines: &[&str],
    query: &str,
    path: &Path,
    file_lines: u32,
    mtime: SystemTime,
    defs: &mut Vec<Match>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "section" => {
                emit_md_section_match(child, lines, query, path, file_lines, mtime, defs);
                walk_md_sections(child, lines, query, path, file_lines, mtime, defs);
            }
            // The parser owns these — no headings hide inside.
            "fenced_code_block" | "indented_code_block" | "html_block" => {}
            _ => walk_md_sections(child, lines, query, path, file_lines, mtime, defs),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_md_section_match(
    section: tree_sitter::Node,
    lines: &[&str],
    query: &str,
    path: &Path,
    file_lines: u32,
    mtime: SystemTime,
    defs: &mut Vec<Match>,
) {
    let mut cursor = section.walk();
    let Some(heading) = section
        .children(&mut cursor)
        .find(|c| c.kind() == "atx_heading")
    else {
        return;
    };
    let text = heading_text(heading, lines);
    if !contains_identifier(&text, query) {
        return;
    }
    let heading_line = (heading.start_position().row + 1) as u32;
    let raw_end = md_section_end_line(section);
    let section_end = trim_trailing_blank_lines(lines, heading_line, raw_end);
    let line_text = lines
        .get(heading.start_position().row)
        .copied()
        .unwrap_or("");
    defs.push(Match {
        path: path.to_path_buf(),
        line: heading_line,
        text: line_text.trim_end().to_string(),
        is_definition: true,
        exact: true,
        file_lines,
        mtime,
        // Populating def_range lets the renderer expand to the section
        // body — the markdown analogue of a code definition's body.
        def_range: Some((heading_line, section_end)),
        def_name: Some(query.to_string()),
        // Soft definition — code definitions are 60-80, usages 0. Sits
        // between them so docs headings outrank passing mentions but
        // never outrank the real source.
        def_weight: 30,
        impl_target: None,
    });
}

/// 1-indexed inclusive last line of a tree-sitter section node.
fn md_section_end_line(section: tree_sitter::Node) -> u32 {
    let end = section.end_position();
    if end.column == 0 {
        end.row as u32
    } else {
        (end.row + 1) as u32
    }
}

fn trim_trailing_blank_lines(lines: &[&str], start: u32, end: u32) -> u32 {
    let mut e = end;
    while e > start
        && lines
            .get((e - 1) as usize)
            .is_some_and(|l| l.trim().is_empty())
    {
        e -= 1;
    }
    e
}

/// True if `query` appears in `text` as a whole identifier — flanked by
/// non-word characters (anything outside `[A-Za-z0-9_]`) or string ends.
fn contains_identifier(text: &str, query: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    text.match_indices(query).any(|(abs, _)| {
        let bytes = text.as_bytes();
        let before_ok = abs == 0 || !is_word_byte(bytes[abs - 1]);
        let end_pos = abs + query.len();
        let after_ok = end_pos == bytes.len() || !is_word_byte(bytes[end_pos]);
        before_ok && after_ok
    })
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
    use std::fmt::Write as _;
    use std::path::PathBuf;
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
        let ts_lang =
            tilth_core::lang::outline::outline_language(tilth_core::types::Lang::Rust).unwrap();

        let defs = find_defs_treesitter(
            std::path::Path::new("test.rs"),
            "hello",
            &ts_lang,
            Some(tilth_core::types::Lang::Rust),
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
            Some(tilth_core::types::Lang::Rust),
            code,
            15,
            SystemTime::now(),
        );
        assert!(!defs.is_empty(), "should find 'Foo' definition");

        let defs = find_defs_treesitter(
            std::path::Path::new("test.rs"),
            "dispatch_tool",
            &ts_lang,
            Some(tilth_core::types::Lang::Rust),
            code,
            15,
            SystemTime::now(),
        );
        assert!(!defs.is_empty(), "should find 'dispatch_tool' definition");
    }

    #[test]
    fn typescript_export_const_detected_as_definition() {
        let code = r#"export const UNTAGGED_REQUESTS_SQL = `SELECT foo FROM bar`;

export const anotherConst = 42;

const unexported = "hello";
"#;
        let ts_lang =
            tilth_core::lang::outline::outline_language(tilth_core::types::Lang::TypeScript)
                .unwrap();
        let lines = code.lines().count() as u32;

        let defs = find_defs_treesitter(
            std::path::Path::new("test.ts"),
            "UNTAGGED_REQUESTS_SQL",
            &ts_lang,
            Some(tilth_core::types::Lang::TypeScript),
            code,
            lines,
            SystemTime::now(),
        );
        assert!(
            !defs.is_empty(),
            "should find 'UNTAGGED_REQUESTS_SQL' definition"
        );
        assert!(defs[0].is_definition);
        assert!(defs[0].def_range.is_some());

        // Non-exported const also detected
        let defs = find_defs_treesitter(
            std::path::Path::new("test.ts"),
            "unexported",
            &ts_lang,
            Some(tilth_core::types::Lang::TypeScript),
            code,
            lines,
            SystemTime::now(),
        );
        assert!(!defs.is_empty(), "should find 'unexported' definition");
        assert!(defs[0].is_definition);
    }

    /// Helper: search for an Elixir definition by name in a code snippet.
    fn elixir_find(code: &str, name: &str) -> Vec<Match> {
        let ts_lang =
            tilth_core::lang::outline::outline_language(tilth_core::types::Lang::Elixir).unwrap();
        let lines = code.lines().count() as u32;
        find_defs_treesitter(
            std::path::Path::new("test.ex"),
            name,
            &ts_lang,
            Some(tilth_core::types::Lang::Elixir),
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
        let code = r"defmodule Guards do
  def safe_div(a, b) when b != 0 do
    a / b
  end

  defp checked(x) when is_integer(x), do: x

  defguard is_positive(x) when x > 0
end
";
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
        let code = r"defprotocol Printable do
  @callback format(t) :: String.t()
  def to_string(data)
end

defimpl Printable, for: User do
  def to_string(user), do: user.name
end

defmodule MyError do
  defexception [:message, :code]
end
";
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
        let code = r"defmodule Outer do
  defdelegate count(list), to: Enum

  defmodule Inner do
    def nested_func, do: :ok
  end
end
";
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

    fn md_find(content: &str, query: &str) -> Vec<Match> {
        let lines = content.lines().count() as u32;
        find_defs_markdown_buf(
            std::path::Path::new("test.md"),
            query,
            content,
            lines,
            SystemTime::now(),
        )
    }

    #[test]
    fn markdown_heading_named_for_query_matches() {
        let content = "# Intro\n\n## parseCitations\n\nProse.\n";
        let defs = md_find(content, "parseCitations");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].line, 3);
        assert!(defs[0].is_definition);
        assert_eq!(defs[0].def_weight, 30);
    }

    #[test]
    fn markdown_heading_levels_one_through_six() {
        for level in 1..=6 {
            let hashes = "#".repeat(level);
            let content = format!("{hashes} parseCitations\n");
            assert_eq!(md_find(&content, "parseCitations").len(), 1, "h{level}");
        }
        // h7 is not a heading
        assert!(md_find("####### parseCitations\n", "parseCitations").is_empty());
    }

    #[test]
    fn markdown_heading_without_query_does_not_match() {
        let content = "## Other section\n\n## Another heading\n";
        assert!(md_find(content, "parseCitations").is_empty());
    }

    #[test]
    fn markdown_substring_inside_word_does_not_match() {
        // query "func" must not match "function" — that's the maintainer's
        // word-boundary concern. Same for "factor" inside "refactoring".
        assert!(md_find("## function pointers\n", "func").is_empty());
        assert!(md_find("## refactoring guidelines\n", "factor").is_empty());
        assert!(md_find("## getCitationsBatch\n", "Citations").is_empty());
    }

    #[test]
    fn markdown_whole_word_in_phrase_matches() {
        // Whole-word match anywhere in the heading text is a definition —
        // a heading like `## How parseCitations works` IS naming the symbol.
        let defs = md_find("## How parseCitations works\n", "parseCitations");
        assert_eq!(defs.len(), 1);
    }

    #[test]
    fn markdown_query_with_hyphen_matches() {
        // Tracking-doc identifiers like `GUM-1732` must match. The hyphen
        // is part of the query; word-boundary check applies only at the ends.
        let defs = md_find("## GUM-1732: Cost attribution\n", "GUM-1732");
        assert_eq!(defs.len(), 1);
    }

    #[test]
    fn markdown_code_block_lines_do_not_match() {
        // Fenced code block — line is not an ATX heading, even though
        // the text contains `function parseCitations`.
        let content = "## Real heading\n\n```ts\nfunction parseCitations() {}\n```\n";
        let defs = md_find(content, "parseCitations");
        assert!(defs.is_empty(), "fenced-code mention is not a definition");

        // Indented code block (4+ space indent) — a `## ...` line indented
        // 4 spaces is a code block per CommonMark, not a heading.
        let content = "Intro.\n\n    ## parseCitations\n";
        assert!(
            md_find(content, "parseCitations").is_empty(),
            "4-space-indented `## foo` is a code block, not a heading"
        );
    }

    #[test]
    fn markdown_heading_with_up_to_three_space_indent_matches() {
        // 0-3 space indents are valid ATX headings per CommonMark.
        for indent in 0..=3 {
            let content = format!("{}## parseCitations\n", " ".repeat(indent));
            assert_eq!(
                md_find(&content, "parseCitations").len(),
                1,
                "indent {indent} should be a heading"
            );
        }
    }

    #[test]
    fn markdown_heading_with_trailing_hashes_matches() {
        // ATX allows optional trailing `#`s — strip them before matching.
        assert_eq!(md_find("## parseCitations ##\n", "parseCitations").len(), 1);
        assert_eq!(
            md_find("### parseCitations ###\n", "parseCitations").len(),
            1
        );
    }

    #[test]
    fn markdown_hashes_without_space_are_not_headings() {
        // `##foo` (no space after `#`s) is not a heading.
        assert!(md_find("##parseCitations\n", "parseCitations").is_empty());
    }

    #[test]
    fn markdown_section_span_runs_to_next_same_level_heading() {
        // `## parseCitations` body ends at the next `## ...` (same level).
        // The blank line on line 4 (between body and next heading) is
        // trimmed, so the span ends at line 3.
        let content = "\
## parseCitations

Body line.

## Other section

Unrelated.
";
        let defs = md_find(content, "parseCitations");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].line, 1);
        assert_eq!(defs[0].def_range, Some((1, 3)));
    }

    #[test]
    fn markdown_section_span_runs_to_higher_level_heading() {
        // A `## ...` ends a sub-section under `### parseCitations` because
        // the outer heading is higher level (smaller hash count). The blank
        // line preceding `## Outer two` is trimmed.
        let content = "\
## Outer

### parseCitations

Body.

## Outer two
";
        let defs = md_find(content, "parseCitations");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].line, 3);
        assert_eq!(defs[0].def_range, Some((3, 5)));
    }

    #[test]
    fn markdown_section_span_skips_deeper_subheadings() {
        // A `### ...` does NOT end the enclosing `## parseCitations`
        // section — only same-or-higher-level headings do.
        let content = "\
## parseCitations

Lead-in.

### Detail

Subprose.

## Next
";
        let defs = md_find(content, "parseCitations");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].line, 1);
        assert_eq!(defs[0].def_range, Some((1, 7)));
    }

    #[test]
    fn markdown_section_span_runs_to_eof_when_no_following_heading() {
        let content = "\
## parseCitations

Body to end.
";
        let defs = md_find(content, "parseCitations");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].line, 1);
        // Three content lines; trailing newline does not produce a 4th.
        assert_eq!(defs[0].def_range, Some((1, 3)));
    }

    #[test]
    fn markdown_section_span_handles_heading_with_no_body() {
        // Adjacent headings: span is just the heading line itself.
        let content = "\
## parseCitations
## Other
";
        let defs = md_find(content, "parseCitations");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].line, 1);
        assert_eq!(defs[0].def_range, Some((1, 1)));
    }

    #[test]
    fn stratify_for_display_keeps_code_defs_above_doc_defs() {
        // When the cap drops matches, real code defs must keep their slots
        // and doc-heading defs slide below them. Rank order within each
        // stratum is preserved by the stable sort.
        let mk = |line: u32, weight: u16, is_definition: bool| Match {
            path: PathBuf::from("test.rs"),
            line,
            text: String::new(),
            is_definition,
            exact: false,
            file_lines: 100,
            mtime: SystemTime::now(),
            def_range: None,
            def_name: None,
            def_weight: weight,
            impl_target: None,
        };

        // Pre-cap order (after rank::sort): doc def, code def, doc def, code def, usage.
        let mut matches = vec![
            mk(1, 30, true), // doc def — high relevance
            mk(2, 70, true), // code def
            mk(3, 30, true), // doc def
            mk(4, 70, true), // code def
            mk(5, 0, false), // usage
        ];
        matches.sort_by_key(stratum_for_display);

        // Code defs first (stable order: line 2 before line 4),
        // then doc defs (line 1 before line 3), then the usage.
        let lines: Vec<u32> = matches.iter().map(|m| m.line).collect();
        assert_eq!(lines, vec![2, 4, 1, 3, 5]);

        // Truncate-to-2 should keep both code defs, drop both doc defs.
        matches.truncate(2);
        assert!(
            matches.iter().all(|m| m.def_weight >= 60),
            "displayed slice after cap must be all code defs, got {:?}",
            matches.iter().map(|m| m.def_weight).collect::<Vec<_>>()
        );
    }

    /// The old walkers quit on a shared counter (50 definitions, 30 usages),
    /// so the total was however far the threads got: "10 of 48" for a symbol
    /// with 685 hits, and a different number on each run. 80 files with one
    /// definition and two usages each are well past both thresholds.
    #[test]
    fn symbol_totals_are_exact_and_identical_across_runs() {
        let dir = tempfile::tempdir().expect("tempdir");
        for i in 0..80 {
            std::fs::write(
                dir.path().join(format!("f{i:02}.rs")),
                format!("pub fn exact_needle_{i}() {{}}\nfn exact_needle() {{}}\nfn user_{i}() {{ exact_needle(); exact_needle(); }}\n"),
            )
            .expect("write");
        }
        let first = search("exact_needle", dir.path(), None, None, false).expect("search");
        assert_eq!(first.definitions, 80, "every definition is counted");
        assert_eq!(first.usages, 80, "every usage line is counted");
        assert_eq!(first.total_found, 160);
        assert_eq!(
            first.facet_totals.definitions
                + first.facet_totals.usages_local
                + first.facet_totals.usages_cross
                + first.facet_totals.tests
                + first.facet_totals.implementations,
            160,
            "facet totals add up to the exact total"
        );
        let shown = |r: &SearchResult| -> Vec<(PathBuf, u32)> {
            r.matches.iter().map(|m| (m.path.clone(), m.line)).collect()
        };
        for _ in 0..4 {
            let again = search("exact_needle", dir.path(), None, None, false).expect("search");
            assert_eq!(again.total_found, first.total_found);
            assert_eq!(shown(&again), shown(&first), "same matches every run");
        }
    }

    /// The collection bound must be invisible in the result: at the
    /// production bound and with no bound, the totals, the facet totals and
    /// the matches shown are the same. A bound of one shows it is live.
    #[test]
    fn collection_bound_does_not_change_the_result() {
        let dir = tempfile::tempdir().expect("tempdir");
        for f in 0..6 {
            // Comment mentions rank below a call, and the call is the last
            // line, so keeping a file's first matches instead of its best
            // would drop it.
            let mut body = String::new();
            if f % 2 == 0 {
                body.push_str("pub fn bound_probe() {}\n");
            }
            for i in 0..13 {
                let _ = writeln!(body, "// bound_probe note {i}");
            }
            body.push_str("fn user() { bound_probe(); }\n");
            let sub = dir.path().join(format!("pkg{f}"));
            std::fs::create_dir(&sub).expect("mkdir");
            std::fs::write(sub.join("lib.rs"), body).expect("write");
        }
        let key = |r: &SearchResult| -> Vec<(PathBuf, u32, bool)> {
            r.matches
                .iter()
                .map(|m| (m.path.clone(), m.line, m.is_definition))
                .collect()
        };
        let run = |cap| search_capped("bound_probe", dir.path(), None, None, 10, cap).unwrap();
        let bounded = run(10);
        let unbounded = run(usize::MAX);
        assert_eq!(bounded.total_found, 87);
        assert!(
            bounded
                .matches
                .iter()
                .any(|m| m.text.starts_with("fn user()")),
            "a file's best usage is its last line"
        );
        assert_eq!(bounded.total_found, unbounded.total_found);
        assert_eq!(bounded.definitions, 3);
        assert_eq!(key(&bounded), key(&unbounded));
        assert_eq!(
            format!("{:?}", bounded.facet_totals),
            format!("{:?}", unbounded.facet_totals)
        );
        let one = run(1);
        assert_eq!(one.matches.len(), 1, "one match is kept");
        assert_eq!(
            (&one.matches[0].path, one.matches[0].line),
            (&bounded.matches[0].path, bounded.matches[0].line),
            "the best one"
        );
        assert_eq!(one.total_found, 87, "and the count stays exact");
    }

    #[test]
    fn full_flag_raises_match_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let scope = dir.path();

        // Create 15 Rust files each defining WidelyUsedThing.
        for i in 0..15 {
            let path = scope.join(format!("file_{i:02}.rs"));
            std::fs::write(&path, "pub fn WidelyUsedThing() {}\n").expect("write");
        }

        let result_default =
            search("WidelyUsedThing", scope, None, None, false).expect("search default");
        let result_full = search("WidelyUsedThing", scope, None, None, true).expect("search full");

        // Default cap is 10 — should not exceed it.
        assert!(
            result_default.matches.len() <= 10,
            "default: expected ≤10 matches, got {}",
            result_default.matches.len()
        );
        // Full cap is 100 — all 15 definitions should be visible.
        assert!(
            result_full.matches.len() > 10,
            "full: expected >10 matches, got {}",
            result_full.matches.len()
        );
        // total_found is measured pre-truncation and should be equal.
        assert_eq!(
            result_default.total_found, result_full.total_found,
            "total_found must be the same regardless of full flag"
        );
    }

    /// Search the Go declarations fixture end to end and return every match
    /// on `line`.
    fn go_fixture_matches_on_line(query: &str, line: u32) -> Vec<Match> {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("main.go"),
            tilth_core::lang::go::GO_DECLARATIONS_FIXTURE,
        )
        .expect("write fixture");
        let result = search(query, dir.path(), None, None, false).expect("search");
        result
            .matches
            .into_iter()
            .filter(|m| m.line == line)
            .collect()
    }

    #[test]
    fn go_type_and_grouped_const_are_definitions() {
        // `type Server struct` (line 9) and `StatusActive` inside the grouped
        // `const ( ... )` block (line 4) are definitions, each ranged to its
        // own spec, with no leftover usage match on the same line.
        for (query, line) in [("Server", 9), ("StatusActive", 4)] {
            let on_line = go_fixture_matches_on_line(query, line);
            assert_eq!(
                on_line.len(),
                1,
                "{query}: expected one match on line {line}, got {on_line:?}"
            );
            let m = &on_line[0];
            assert!(
                m.is_definition,
                "{query} on line {line} must be a definition"
            );
            assert_eq!(m.def_range, Some((line, line)), "{query} def_range");
        }
    }

    /// Find Go definitions of `name` in `code` with the tree-sitter walker.
    fn go_find(code: &str, name: &str) -> Vec<Match> {
        let ts_lang =
            tilth_core::lang::outline::outline_language(tilth_core::types::Lang::Go).unwrap();
        find_defs_treesitter(
            std::path::Path::new("main.go"),
            name,
            &ts_lang,
            Some(tilth_core::types::Lang::Go),
            code,
            code.lines().count() as u32,
            SystemTime::now(),
        )
    }

    #[test]
    fn go_package_level_declarations_detected() {
        let code = tilth_core::lang::go::GO_DECLARATIONS_FIXTURE;
        for (name, line) in [
            ("StatusInactive", 5),
            ("Version", 7),
            ("GlobalCounter", 8),
            ("NewServer", 10),
        ] {
            let defs = go_find(code, name);
            assert_eq!(
                defs.len(),
                1,
                "{name}: expected one definition, got {defs:?}"
            );
            assert_eq!(defs[0].line, line, "{name} definition line");
        }
    }

    #[test]
    fn go_multi_name_specs_define_every_name() {
        // `var y, z = 1, 2` and `const A, B = 1, 2` bind each name, and a
        // `type ( ... )` group defines each of its types.
        let code = "package p

const A, B = 1, 2

var (
\tx int
\ty, z = 1, 2
)

type (
\tID int
\tAlias = string
)
";
        for (name, line) in [
            ("A", 3),
            ("B", 3),
            ("x", 6),
            ("y", 7),
            ("z", 7),
            ("ID", 11),
            ("Alias", 12),
        ] {
            let defs = go_find(code, name);
            assert_eq!(
                defs.len(),
                1,
                "{name}: expected one definition, got {defs:?}"
            );
            assert_eq!(defs[0].line, line, "{name} definition line");
        }
    }

    #[test]
    fn go_function_local_declarations_are_not_definitions() {
        // Only package-level declarations are definitions. A `var`, `const` or
        // `type` inside a function body is local to that function.
        let code = "package p

func run() {
\tvar local = 1
\tconst limit = 2
\ttype pair struct{}
\t_ = local
}
";
        for name in ["local", "limit", "pair"] {
            assert!(
                go_find(code, name).is_empty(),
                "{name} is function-local and must not be a definition"
            );
        }
        assert_eq!(go_find(code, "run").len(), 1, "run is a definition");
    }

    /// Search `fixture`, written to `file`, for `query` end to end and return
    /// the line of every definition found, sorted. Fails if a definition's
    /// `def_name` is not the bare query.
    fn definition_lines(file: &str, fixture: &str, query: &str) -> Vec<u32> {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(file), fixture).expect("write fixture");
        let result = search(query, dir.path(), None, None, false).expect("search");
        let mut lines: Vec<u32> = result
            .matches
            .iter()
            .filter(|m| m.is_definition)
            .inspect(|m| assert_eq!(m.def_name.as_deref(), Some(query), "{query} def_name"))
            .map(|m| m.line)
            .collect();
        lines.sort_unstable();
        lines
    }

    #[test]
    fn c_definitions_are_found_by_their_bare_name() {
        // Each function is a definition on its own line, whatever wraps the
        // name: a plain, pointer, or function-pointer return type. The
        // prototype of `add` (line 9) is not a definition, nor is the forward
        // `struct point;` (line 3) or the bodiless `struct point` return type
        // of `origin` (line 17).
        let fixture = tilth_core::lang::c::C_DEFINITIONS_FIXTURE;
        for (query, expected) in [
            ("add", vec![11]),
            ("make", vec![14]),
            ("label", vec![15]),
            ("pick", vec![16]),
            ("origin", vec![17]),
            ("main", vec![18]),
            ("point", vec![4]),
            ("number", vec![5]),
            ("color", vec![6]),
            ("point_t", vec![7]),
            ("compare_fn", vec![8]),
        ] {
            assert_eq!(
                definition_lines("shapes.c", fixture, query),
                expected,
                "{query}: definition lines"
            );
        }
    }

    #[test]
    fn c_function_definition_spans_its_body() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("shapes.c"),
            tilth_core::lang::c::C_DEFINITIONS_FIXTURE,
        )
        .expect("write fixture");
        let result = search("add", dir.path(), None, None, false).expect("search");
        let def = result
            .matches
            .iter()
            .find(|m| m.is_definition)
            .expect("add has a definition");
        assert_eq!(def.def_range, Some((11, 13)));
        assert_eq!(
            def.def_weight, 100,
            "a function ranks as a primary definition"
        );
    }

    #[test]
    fn cpp_definitions_are_found_by_their_bare_name() {
        // In-class methods, out-of-class `Type::name` definitions, functions
        // in nested namespaces, templates and their specialisations, and
        // `extern "C"` functions are definitions under their bare name.
        // In-class declarations (lines 4, 8), the namespace-level prototype
        // (line 14) and the forward `class Forward;` (line 12) are not. A
        // constructor defines its class's name too (line 10).
        let fixture = tilth_core::lang::cpp::CPP_DEFINITIONS_FIXTURE;
        for (query, expected) in [
            ("ref", vec![1]),
            ("Shape", vec![2, 10]),
            ("area", vec![6]),
            ("sides", vec![11]),
            ("Forward", vec![]),
            ("geo", vec![13]),
            ("helper", vec![15]),
            ("biggest", vec![16, 21]),
            ("Box", vec![17]),
            ("volume", vec![18]),
            ("c_entry", vec![22]),
            ("Size", vec![23]),
            ("Mode", vec![24]),
        ] {
            assert_eq!(
                definition_lines("shapes.cpp", fixture, query),
                expected,
                "{query}: definition lines"
            );
        }
    }

    #[test]
    fn cpp_destructor_and_operator_definitions_keep_their_spelling() {
        // `~Shape` and `operator==` are not words the usage regex can match, so
        // check the definition walker directly.
        let code = tilth_core::lang::cpp::CPP_DEFINITIONS_FIXTURE;
        let ts_lang =
            tilth_core::lang::outline::outline_language(tilth_core::types::Lang::Cpp).unwrap();
        for (name, line) in [("~Shape", 5), ("operator==", 7)] {
            let defs = find_defs_treesitter(
                std::path::Path::new("shapes.cpp"),
                name,
                &ts_lang,
                Some(tilth_core::types::Lang::Cpp),
                code,
                code.lines().count() as u32,
                SystemTime::now(),
            );
            let lines: Vec<u32> = defs.iter().map(|m| m.line).collect();
            assert_eq!(lines, vec![line], "{name}: definition lines");
        }
    }

    #[test]
    fn csharp_method_in_namespaced_class_is_a_definition() {
        // A namespace adds no nesting of its own, so a method in a class in a
        // namespace is found like a method in a top-level class.
        let code = "namespace App {\n  public class Svc {\n    public void Run() {}\n  }\n}\n";
        assert_eq!(definition_lines("Svc.cs", code, "Svc"), vec![2]);
        assert_eq!(definition_lines("Svc.cs", code, "Run"), vec![3]);
    }
}
