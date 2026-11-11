//! Imports as data, one per module a statement names, and their resolution
//! to local file paths (the MCP layer's related-file hint, deps, callees).

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use streaming_iterator::StreamingIterator;

use crate::lang::detect_file_type_with_content;
use crate::types::{FileType, Lang};

/// The local files a code file's imports resolve to, read through `cache`'s
/// parsed tree so an outlined read does not parse the file a second time.
/// Returns empty for non-code files, files with no imports, or when all
/// imports are external.
#[must_use]
pub fn resolve_related_files(file_path: &Path, cache: &crate::cache::OutlineCache) -> Vec<PathBuf> {
    if let Some(parsed) = cache.get_or_parse(file_path) {
        let imports = imports_in_tree(parsed.tree.root_node(), &parsed.content, parsed.lang);
        return resolve_imports(file_path, parsed.lang, &imports);
    }
    // The cache holds no tree for this file (no grammar, or over its size
    // cap): read it and parse here.
    let Ok(content) = crate::source::read_source(file_path) else {
        return Vec::new();
    };
    resolve_related_files_with_content(file_path, &content)
}

/// Same as `resolve_related_files` but takes pre-read content to avoid a redundant file read.
#[must_use]
pub fn resolve_related_files_with_content(file_path: &Path, content: &str) -> Vec<PathBuf> {
    let FileType::Code(lang) = detect_file_type_with_content(file_path, content) else {
        return Vec::new();
    };
    resolve_imports(file_path, lang, &extract_imports(content, lang))
}

/// Resolve the local imports of the file at `file_path` to existing files,
/// each once, in import order. For a caller that already holds the file's
/// imports.
#[must_use]
pub fn resolve_imports(file_path: &Path, lang: Lang, imports: &[Import]) -> Vec<PathBuf> {
    let mut results = Vec::new();
    for import in imports {
        if let Some(path) = resolve_import(file_path, lang, import) {
            if !results.contains(&path) {
                results.push(path);
            }
        }
    }
    results
}

/// The local file one import of the file at `file_path` names, if it
/// resolves to one. A Python absolute import (`from pkg.mod import x`) is
/// local when `pkg/mod.py` sits under a directory above the importer, up
/// to its project root; `is_external` cannot tell that without the disk.
#[must_use]
pub fn resolve_import(file_path: &Path, lang: Lang, import: &Import) -> Option<PathBuf> {
    let dir = file_path.parent()?;
    if is_external(&import.source, lang) && lang != Lang::Python {
        return None;
    }
    resolve(dir, &import.source, lang)
}

/// One module an import statement names.
///
/// A statement can name several modules: a Go `import ( … )` block, a Rust
/// `use a::{b, c}` group, Python's `import os, json`, a Scala
/// `import a.{b, c}`, an Elixir `alias A.{B, C}`. Each module is its own
/// `Import`, on the line that names it, and carries the lines of the
/// statement it sits in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// The module as the file spells it, without an alias: `example.com/m/core`,
    /// `crate::seams::git`, `json`, `./core`, `"util.h"`. `is_external` reads it.
    pub source: String,
    /// The 1-based line on which the statement names this module.
    pub line: u32,
    /// The 1-based first line of the statement.
    pub start_line: u32,
    /// The 1-based last line of the statement, inclusive.
    pub end_line: u32,
}

/// Every module `content` imports, one [`Import`] per module, in source order.
///
/// In Go, Rust, Python, JavaScript, TypeScript, Scala, Elixir, Java,
/// Kotlin, PHP, C# and Swift, statements come from the syntax tree, so an
/// import-shaped line inside a string is not one and an alias is not part
/// of a source. A JavaScript or TypeScript re-export (`export … from "m"`)
/// imports `m` too. In C,
/// C++ and Bash, where an import is one line naming one file, each line
/// `is_import_line` accepts gives one import, its source read by
/// `extract_import_source`. A statement that does not spell its module
/// out, such as a shell `source "$dir/lib.sh"`, gives none. Languages without
/// imports tilth reads give none.
#[must_use]
pub fn extract_imports(content: &str, lang: Lang) -> Vec<Import> {
    if !reads_statements_from_tree(lang) {
        return line_imports(content, lang);
    }
    let Some(language) = crate::lang::outline::outline_language(lang) else {
        return Vec::new();
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };
    imports_in_tree(tree.root_node(), content, lang)
}

/// [`extract_imports`] over a tree the caller has already parsed from
/// `content` with `lang`'s grammar, so a caller that has one parses once.
#[must_use]
pub fn imports_in_tree(root: tree_sitter::Node, content: &str, lang: Lang) -> Vec<Import> {
    if !reads_statements_from_tree(lang) {
        return line_imports(content, lang);
    }
    let Some(query) = statement_query(lang) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = tree_sitter::QueryCursor::new();
    // These grammars allow imports only at the top of a file, so the match
    // need not look below the root's children.
    if matches!(
        lang,
        Lang::Go | Lang::TypeScript | Lang::Tsx | Lang::JavaScript | Lang::Java | Lang::Kotlin
    ) {
        cursor.set_max_start_depth(Some(1));
    }
    let mut matches = cursor.matches(query, root, content.as_bytes());
    while let Some(found) = matches.next() {
        for capture in found.captures() {
            let node = capture.node;
            for (source, line) in statement_modules(node, content, lang).into_iter().flatten() {
                if source.is_empty() {
                    continue;
                }
                out.push(Import {
                    source,
                    line,
                    start_line: line_of(node),
                    end_line: last_line_of(node),
                });
            }
        }
    }
    out
}

/// The query matching the node kinds that can be an import statement in
/// `lang`, compiled on first use and kept for the life of the process.
/// Matching runs inside tree-sitter, which is faster than stepping a cursor
/// over every node from Rust. `statement_modules` then reads each candidate,
/// and drops the Elixir calls that are not imports. Each language compiles
/// only when a file of it is read, since compiling costs a few milliseconds.
fn statement_query(lang: Lang) -> Option<&'static tree_sitter::Query> {
    static COMPILED: LazyLock<Mutex<HashMap<Lang, &'static tree_sitter::Query>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    let pattern = match lang {
        Lang::Go | Lang::Scala | Lang::Java | Lang::Swift => "(import_declaration) @statement",
        Lang::Rust => "(use_declaration) @statement",
        Lang::Python => {
            "[(import_statement) (import_from_statement) (future_import_statement)] @statement"
        }
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
            "[(import_statement) (export_statement source: (_))] @statement"
        }
        Lang::Elixir => "(call target: (identifier)) @statement",
        Lang::Kotlin => "(import) @statement",
        Lang::Php => "(namespace_use_declaration) @statement",
        Lang::CSharp => "(using_directive) @statement",
        _ => return None,
    };
    let mut compiled = COMPILED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match compiled.entry(lang) {
        Entry::Occupied(e) => Some(*e.get()),
        Entry::Vacant(e) => {
            let language = crate::lang::outline::outline_language(lang)?;
            // One query per language at most, so leaking it to get a
            // `'static` borrow is bounded, and matching then runs without
            // the lock.
            let query = Box::leak(Box::new(tree_sitter::Query::new(&language, pattern).ok()?));
            Some(*e.insert(query))
        }
    }
}

/// Languages whose import statements are read from the syntax tree: one
/// statement can name several modules, span lines, or carry an alias or
/// modifier that is not part of the module.
fn reads_statements_from_tree(lang: Lang) -> bool {
    matches!(
        lang,
        Lang::Go
            | Lang::Rust
            | Lang::Python
            | Lang::TypeScript
            | Lang::Tsx
            | Lang::JavaScript
            | Lang::Scala
            | Lang::Elixir
            | Lang::Java
            | Lang::Kotlin
            | Lang::Php
            | Lang::CSharp
            | Lang::Swift
    )
}

/// One import per line `is_import_line` accepts.
fn line_imports(content: &str, lang: Lang) -> Vec<Import> {
    let mut out = Vec::new();
    for (index, line) in content.lines().enumerate() {
        if !is_import_line(line, lang) {
            continue;
        }
        let source = crate::lang::outline::extract_import_source(line, Some(lang));
        if source.is_empty() {
            continue;
        }
        let line = index as u32 + 1;
        out.push(Import {
            source,
            line,
            start_line: line,
            end_line: line,
        });
    }
    out
}

/// The modules an import statement names, each with the line it is named on.
/// `None` when `node` is not an import statement.
fn statement_modules(node: tree_sitter::Node, src: &str, lang: Lang) -> Option<Vec<(String, u32)>> {
    let mut out = Vec::new();
    match (lang, node.kind()) {
        (Lang::Go, "import_declaration") => go_specs(node, src, &mut out),
        (Lang::Rust, "use_declaration") => {
            if let Some(tree) = node.child_by_field_name("argument") {
                rust_use_tree(tree, "", src, &mut out);
            }
        }
        (Lang::Python, "import_statement") => {
            let mut cursor = node.walk();
            for name in node.children_by_field_name("name", &mut cursor) {
                // `import a.b as c` names `a.b`.
                let module = name.child_by_field_name("name").unwrap_or(name);
                out.push((compact(module, src), line_of(module)));
            }
        }
        (Lang::Python, "import_from_statement") => {
            if let Some(module) = node.child_by_field_name("module_name") {
                out.push((compact(module, src), line_of(module)));
            }
        }
        (Lang::Python, "future_import_statement") => {
            out.push(("__future__".to_string(), line_of(node)));
        }
        (Lang::TypeScript | Lang::Tsx | Lang::JavaScript, "export_statement") => {
            // Only a re-export names a module: `export { x } from "m"`,
            // `export * from "m"`, `export * as ns from "m"`.
            if let Some(source) = node.child_by_field_name("source") {
                out.push((unquote(source, src), line_of(source)));
            }
        }
        (Lang::TypeScript | Lang::Tsx | Lang::JavaScript, "import_statement") => {
            // `import x from "m"` and `import "m"` carry the source on the
            // statement; `import x = require("m")` on its require clause.
            let source = node.child_by_field_name("source").or_else(|| {
                let mut cursor = node.walk();
                let clause = node
                    .children(&mut cursor)
                    .find(|c| c.kind() == "import_require_clause");
                clause.and_then(|c| c.child_by_field_name("source"))
            });
            if let Some(source) = source {
                out.push((unquote(source, src), line_of(source)));
            }
        }
        (Lang::Scala, "import_declaration") => scala_paths(node, src, &mut out),
        (Lang::Java, "import_declaration") | (Lang::Kotlin, "import") => {
            dotted_import(node, src, &mut out);
        }
        (Lang::Elixir, "call") => elixir_directive(node, src, &mut out)?,
        (Lang::Php, "namespace_use_declaration") => php_use(node, src, &mut out),
        (Lang::CSharp, "using_directive") => csharp_using(node, src, &mut out),
        (Lang::Swift, "import_declaration") => {
            let mut cursor = node.walk();
            let path = node
                .named_children(&mut cursor)
                .find(|c| c.kind() == "identifier");
            if let Some(path) = path {
                out.push((compact(path, src), line_of(path)));
            }
        }
        _ => return None,
    }
    Some(out)
}

/// Go: `import "m"` and `import ( "a"; x "b" )`, one module per spec.
fn go_specs(node: tree_sitter::Node, src: &str, out: &mut Vec<(String, u32)>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "import_spec" => {
                if let Some(path) = child.child_by_field_name("path") {
                    out.push((unquote(path, src), line_of(path)));
                }
            }
            "import_spec_list" => go_specs(child, src, out),
            _ => {}
        }
    }
}

/// Rust: expand a use tree into one path per leaf. `crate::{core, seams::git}`
/// gives `crate::core` and `crate::seams::git`; `foo::{self, bar as baz}`
/// gives `foo` and `foo::bar`; a glob keeps its `*`.
fn rust_use_tree(node: tree_sitter::Node, prefix: &str, src: &str, out: &mut Vec<(String, u32)>) {
    match node.kind() {
        "scoped_use_list" => {
            let path = node
                .child_by_field_name("path")
                .map(|p| compact(p, src))
                .unwrap_or_default();
            if let Some(list) = node.child_by_field_name("list") {
                rust_use_tree(list, &join_path(prefix, &path, "::"), src, out);
            }
        }
        "use_list" => {
            let mut cursor = node.walk();
            for member in node.named_children(&mut cursor) {
                rust_use_tree(member, prefix, src, out);
            }
        }
        "use_as_clause" => {
            if let Some(path) = node.child_by_field_name("path") {
                out.push((join_path(prefix, &compact(path, src), "::"), line_of(node)));
            }
        }
        "self" if !prefix.is_empty() => out.push((prefix.to_string(), line_of(node))),
        kind if kind.ends_with("comment") => {}
        _ => out.push((join_path(prefix, &compact(node, src), "::"), line_of(node))),
    }
}

/// Scala: `import a.b, c.d` gives two paths; `import a.{b, c => d}` gives
/// `a.b` and `a.c`; a wildcard keeps its `_`.
fn scala_paths(node: tree_sitter::Node, src: &str, out: &mut Vec<(String, u32)>) {
    /// Close the path read so far; a path that ended in selectors or a
    /// wildcard has already given its imports.
    fn flush(path: &mut String, line: u32, selected: &mut bool, out: &mut Vec<(String, u32)>) {
        let stem = path.trim_end_matches('.');
        if !*selected && !stem.is_empty() {
            out.push((stem.to_string(), line));
        }
        path.clear();
        *selected = false;
    }

    let mut path = String::new();
    let mut path_line = line_of(node);
    let mut selected = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "import" => {}
            "," => flush(&mut path, path_line, &mut selected, out),
            "namespace_selectors" => {
                let stem = path.trim_end_matches('.').to_string();
                let mut members = child.walk();
                for member in child.named_children(&mut members) {
                    if member.kind().ends_with("comment") {
                        continue;
                    }
                    let name = if member.kind() == "namespace_wildcard" {
                        "_".to_string()
                    } else {
                        compact(member.child_by_field_name("name").unwrap_or(member), src)
                    };
                    out.push((join_path(&stem, &name, "."), line_of(member)));
                }
                selected = true;
            }
            "namespace_wildcard" => {
                out.push((
                    join_path(path.trim_end_matches('.'), "_", "."),
                    line_of(child),
                ));
                selected = true;
            }
            kind if kind.ends_with("comment") => {}
            _ => {
                if path.is_empty() {
                    path_line = line_of(child);
                }
                path.push_str(&compact(child, src));
            }
        }
    }
    flush(&mut path, path_line, &mut selected, out);
}

/// Java and Kotlin: one dotted path per statement, a wildcard kept as `.*`.
/// `static` and an `as` alias are not part of the module.
fn dotted_import(node: tree_sitter::Node, src: &str, out: &mut Vec<(String, u32)>) {
    let mut cursor = node.walk();
    let children: Vec<tree_sitter::Node> = node.children(&mut cursor).collect();
    let Some(path) = children.iter().find(|c| {
        matches!(
            c.kind(),
            "scoped_identifier" | "qualified_identifier" | "identifier"
        )
    }) else {
        return;
    };
    let mut source = compact(*path, src);
    if children
        .iter()
        .any(|c| matches!(c.kind(), "asterisk" | "*"))
    {
        source.push_str(".*");
    }
    out.push((source, line_of(*path)));
}

/// Elixir: `alias`, `import`, `use` and `require` name the module in their
/// first argument; `alias A.{B, C}` names `A.B` and `A.C`. `None` when the
/// call is none of the four.
fn elixir_directive(
    node: tree_sitter::Node,
    src: &str,
    out: &mut Vec<(String, u32)>,
) -> Option<()> {
    let target = node.child_by_field_name("target")?;
    if target.kind() != "identifier"
        || !matches!(text(target, src), "alias" | "import" | "use" | "require")
    {
        return None;
    }
    let mut cursor = node.walk();
    let arguments = node
        .children(&mut cursor)
        .find(|c| c.kind() == "arguments")?;
    let mut args = arguments.walk();
    let Some(first) = arguments
        .named_children(&mut args)
        .find(|c| !c.kind().ends_with("comment"))
    else {
        return Some(());
    };
    let group = (first.kind() == "dot")
        .then(|| first.child_by_field_name("right"))
        .flatten()
        .filter(|right| right.kind() == "tuple");
    match (group, first.child_by_field_name("left")) {
        (Some(tuple), Some(left)) => {
            let stem = compact(left, src);
            let mut members = tuple.walk();
            for member in tuple.named_children(&mut members) {
                if !member.kind().ends_with("comment") {
                    out.push((
                        join_path(&stem, &compact(member, src), "."),
                        line_of(member),
                    ));
                }
            }
        }
        _ => out.push((compact(first, src), line_of(first))),
    }
    Some(())
}

/// PHP: `use A\B;`, `use A\B as C, D\E;` and the group `use A\{B, C as D};`,
/// one module per clause, its alias left out. A group's members sit under
/// its prefix. `use function` and `use const` import a function or a
/// constant by its qualified name, which is read the same way.
fn php_use(node: tree_sitter::Node, src: &str, out: &mut Vec<(String, u32)>) {
    fn clause_name(clause: tree_sitter::Node) -> Option<tree_sitter::Node> {
        let mut cursor = clause.walk();
        let found = clause
            .named_children(&mut cursor)
            .find(|c| matches!(c.kind(), "qualified_name" | "name"));
        // `use A\B as C`: the alias is a `name` too, after the module.
        found.filter(|n| clause.child_by_field_name("alias") != Some(*n))
    }

    let prefix = {
        let mut cursor = node.walk();
        let found = node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "namespace_name");
        found.map(|p| compact(p, src))
    };
    // A group's clauses sit in its body; plain clauses on the statement.
    let holder = node.child_by_field_name("body").unwrap_or(node);
    let mut cursor = holder.walk();
    let clauses: Vec<tree_sitter::Node> = holder.named_children(&mut cursor).collect();
    for clause in clauses {
        if clause.kind() != "namespace_use_clause" {
            continue;
        }
        let Some(name) = clause_name(clause) else {
            continue;
        };
        let name = compact(name, src);
        let name = name.trim_start_matches('\\');
        let source = prefix.as_deref().map_or_else(
            || name.to_string(),
            |prefix| join_path(prefix.trim_start_matches('\\'), name, "\\"),
        );
        out.push((source, line_of(clause)));
    }
}

/// C#: `using A.B;`, `using static A.B;` and the alias `using X = A.B;`
/// name the namespace or type `A.B`.
fn csharp_using(node: tree_sitter::Node, src: &str, out: &mut Vec<(String, u32)>) {
    let alias = node.child_by_field_name("name");
    let mut cursor = node.walk();
    let target = node
        .named_children(&mut cursor)
        .find(|c| Some(*c) != alias && !c.kind().ends_with("comment"));
    if let Some(target) = target {
        out.push((compact(target, src), line_of(target)));
    }
}

fn text<'a>(node: tree_sitter::Node, src: &'a str) -> &'a str {
    &src[node.byte_range()]
}

/// A path's text with the whitespace a formatter may put inside it removed.
fn compact(node: tree_sitter::Node, src: &str) -> String {
    text(node, src).split_whitespace().collect()
}

/// A string literal's content.
fn unquote(node: tree_sitter::Node, src: &str) -> String {
    text(node, src)
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .to_string()
}

fn join_path(prefix: &str, rest: &str, separator: &str) -> String {
    if prefix.is_empty() {
        rest.to_string()
    } else if rest.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}{separator}{rest}")
    }
}

fn line_of(node: tree_sitter::Node) -> u32 {
    node.start_position().row as u32 + 1
}

/// The last line a node covers; a node that ends at the start of a line
/// (after its newline) ends on the line before.
fn last_line_of(node: tree_sitter::Node) -> u32 {
    let end = node.end_position();
    if end.column == 0 && end.row > node.start_position().row {
        end.row as u32
    } else {
        end.row as u32 + 1
    }
}

#[must_use]
pub fn is_import_line(line: &str, lang: Lang) -> bool {
    let trimmed = line.trim_start();
    match lang {
        Lang::Rust => trimmed.starts_with("use "),
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
            trimmed.starts_with("import ") || trimmed.starts_with("import{")
        }
        Lang::Python => trimmed.starts_with("import ") || trimmed.starts_with("from "),
        Lang::Go | Lang::Java | Lang::Scala | Lang::Kotlin => trimmed.starts_with("import "),
        Lang::C | Lang::Cpp => trimmed.starts_with("#include"),
        Lang::Elixir => {
            trimmed.starts_with("alias ")
                || trimmed.starts_with("import ")
                || trimmed.starts_with("use ")
                || trimmed.starts_with("require ")
        }
        Lang::Bash => trimmed
            .strip_prefix("source")
            .or_else(|| trimmed.strip_prefix('.'))
            .is_some_and(|rest| rest.starts_with(char::is_whitespace)),
        _ => false,
    }
}

#[must_use]
pub fn is_external(source: &str, lang: Lang) -> bool {
    match lang {
        Lang::Rust => {
            !(source.starts_with("crate::")
                || source.starts_with("self::")
                || source.starts_with("super::"))
        }
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
            !(source.starts_with('.') || source.starts_with("@/") || source.starts_with("~/"))
        }
        // Bash: dot-relative paths are local; anything else (bare name, /abs/path) is external.
        Lang::Python | Lang::Bash => !source.starts_with('.'),
        Lang::C | Lang::Cpp => !source.starts_with('"'),
        // Elixir, Go, Java, Scala, Kotlin — can't resolve without build system knowledge.
        _ => true,
    }
}

fn resolve(dir: &Path, source: &str, lang: Lang) -> Option<PathBuf> {
    let raw = match lang {
        Lang::Rust => resolve_rust(dir, source),
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => resolve_js(dir, source),
        Lang::Python => resolve_python(dir, source),
        Lang::C | Lang::Cpp => resolve_c_include(dir, source),
        Lang::Bash => resolve_bash(dir, source),
        // Elixir, Go, Java, etc. — module-to-file mapping requires build system conventions.
        _ => None,
    };
    raw.map(|p| normalize_path(&p))
}

/// Lexically collapse `.` and `..` components without touching the filesystem.
/// `dir.join("../foo")` returns a `PathBuf` containing literal `..`; without this,
/// distinct spellings of the same target file produce distinct `PathBuf`s and
/// downstream callers (dedup loops, `HashMap` keys) treat them as different files.
fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                let pop_ok = matches!(
                    out.components().next_back(),
                    Some(Component::Normal(_) | Component::Prefix(_))
                );
                if pop_ok {
                    out.pop();
                } else {
                    out.push(comp);
                }
            }
            _ => out.push(comp),
        }
    }
    out
}

// --- Rust ---

fn resolve_rust(dir: &Path, source: &str) -> Option<PathBuf> {
    if let Some(rest) = source.strip_prefix("crate::") {
        let src_dir = find_src_ancestor(dir)?;
        try_rust_path(src_dir, rest)
    } else if let Some(rest) = source.strip_prefix("self::") {
        try_rust_path(dir, rest)
    } else if let Some(rest) = source.strip_prefix("super::") {
        try_rust_path(dir.parent()?, rest)
    } else {
        None
    }
}

/// Try progressively shorter paths until one resolves.
/// `cache::OutlineCache` → try cache/OutlineCache.rs (no) → cache.rs (yes).
/// `read::imports` → try read/imports.rs (yes) → stop.
fn try_rust_path(base: &Path, rest: &str) -> Option<PathBuf> {
    let segments: Vec<&str> = rest.split("::").collect();
    for len in (1..=segments.len()).rev() {
        let rel: PathBuf = segments[..len].iter().collect();
        if let Some(found) = try_rust_module(&base.join(&rel)) {
            return Some(found);
        }
    }
    None
}

fn try_rust_module(base: &Path) -> Option<PathBuf> {
    let with_rs = base.with_extension("rs");
    if with_rs.exists() {
        return Some(with_rs);
    }
    let mod_rs = base.join("mod.rs");
    if mod_rs.exists() {
        return Some(mod_rs);
    }
    None
}

fn find_src_ancestor(start: &Path) -> Option<&Path> {
    let mut current = start;
    loop {
        if current.file_name().and_then(|n| n.to_str()) == Some("src") {
            return Some(current);
        }
        current = current.parent()?;
    }
}

// --- JS/TS ---

fn resolve_js(dir: &Path, source: &str) -> Option<PathBuf> {
    let base = dir.join(source);
    // Try with extensions
    for ext in &[".ts", ".tsx", ".js", ".jsx"] {
        let candidate = PathBuf::from(format!("{}{ext}", base.display()));
        if candidate.exists() {
            return Some(candidate);
        }
    }
    // Already has extension
    if base.exists() && base.is_file() {
        return Some(base);
    }
    // Index files
    for name in &["index.ts", "index.tsx", "index.js", "index.jsx"] {
        let candidate = base.join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

// --- Python ---

fn resolve_python(dir: &Path, source: &str) -> Option<PathBuf> {
    let dots = source.bytes().take_while(|&b| b == b'.').count();
    if dots == 0 {
        return resolve_python_absolute(dir, source);
    }
    // Each dot beyond the first goes up one directory.
    let mut base = dir.to_path_buf();
    for _ in 1..dots {
        base = base.parent()?.to_path_buf();
    }
    let module_part = &source[dots..];
    if module_part.is_empty() {
        // Bare `from . import X`
        let init = base.join("__init__.py");
        return if init.exists() { Some(init) } else { None };
    }
    let rel = module_part.replace('.', "/");
    let as_file = base.join(format!("{rel}.py"));
    if as_file.exists() {
        return Some(as_file);
    }
    let as_pkg = base.join(&rel).join("__init__.py");
    if as_pkg.exists() {
        return Some(as_pkg);
    }
    None
}

/// `a.b.c` as Python finds it from a file in `dir`: `a/b/c.py` or
/// `a/b/c/__init__.py` under `dir` or a directory above it, or under that
/// directory's `src/` (the src layout). The nearest directory wins, as the
/// importer's own directory comes first on Python's path. The search stops
/// at the project root: the first directory holding `.git`,
/// `pyproject.toml`, `setup.py` or `setup.cfg`.
fn resolve_python_absolute(dir: &Path, source: &str) -> Option<PathBuf> {
    const ROOT_MARKERS: [&str; 4] = [".git", "pyproject.toml", "setup.py", "setup.cfg"];
    let mut parts = source.split('.');
    let first = parts.next().filter(|p| !p.is_empty())?;
    let rel: PathBuf = source.split('.').collect();
    for ancestor in dir.ancestors() {
        for base in [ancestor.to_path_buf(), ancestor.join("src")] {
            // One stat rules out most bases: the top-level package or module.
            let top = base.join(first);
            if !top.is_dir() && !top.with_extension("py").is_file() {
                continue;
            }
            let as_file = base.join(&rel).with_extension("py");
            if as_file.is_file() {
                return Some(as_file);
            }
            let as_pkg = base.join(&rel).join("__init__.py");
            if as_pkg.is_file() {
                return Some(as_pkg);
            }
        }
        if ROOT_MARKERS.iter().any(|m| ancestor.join(m).exists()) {
            break;
        }
    }
    None
}

// --- C/C++ ---

fn resolve_c_include(dir: &Path, source: &str) -> Option<PathBuf> {
    let clean = source.trim_matches('"');
    let candidate = dir.join(clean);
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

// --- Bash ---

fn resolve_bash(dir: &Path, source: &str) -> Option<PathBuf> {
    // Only resolve literal relative paths — no extension inference. A single
    // metadata() stat avoids the exists()+is_file() two-call TOCTOU; resolution
    // is best-effort, so a stale result only ever costs a related-file hint.
    let candidate = dir.join(source);
    std::fs::metadata(&candidate)
        .is_ok_and(|m| m.is_file())
        .then_some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn every_tree_read_language_has_a_compiled_statement_query() {
        for lang in [
            Lang::Go,
            Lang::Rust,
            Lang::Python,
            Lang::TypeScript,
            Lang::Tsx,
            Lang::JavaScript,
            Lang::Scala,
            Lang::Elixir,
            Lang::Java,
            Lang::Kotlin,
            Lang::Php,
            Lang::CSharp,
            Lang::Swift,
        ] {
            assert!(reads_statements_from_tree(lang), "{lang:?}");
            assert!(statement_query(lang).is_some(), "{lang:?}");
        }
    }

    fn sources(content: &str, lang: Lang) -> Vec<String> {
        extract_imports(content, lang)
            .into_iter()
            .map(|i| i.source)
            .collect()
    }

    #[test]
    fn import_shaped_text_in_a_string_is_not_an_import() {
        let python = "DOC = \"\"\"\nimport not_a_module\n\"\"\"\nimport real\n";
        assert_eq!(sources(python, Lang::Python), ["real"]);
    }

    #[test]
    fn rust_use_trees_expand_to_leaf_paths() {
        let rust = "use foo::{self, bar as baz, *};\nuse x::*;\nfn f() {\n    use inner::q;\n}\n";
        assert_eq!(
            sources(rust, Lang::Rust),
            ["foo", "foo::bar", "foo::*", "x::*", "inner::q"]
        );
    }

    #[test]
    fn line_languages_give_one_import_per_line() {
        let bash = "source ./lib.sh\nsource \"$DIR/x.sh\"\n";
        assert_eq!(sources(bash, Lang::Bash), ["./lib.sh"]);
        let c = "#include \"util.h\"\n#include <stdio.h>\n";
        assert_eq!(sources(c, Lang::C), ["\"util.h\"", "<stdio.h>"]);
    }

    /// Java's `static` and Kotlin's `as` are not part of the module. Read off
    /// the line, the source was `static a.b.D.e` or `a.b.D as E`.
    #[test]
    fn java_and_kotlin_imports_name_the_module_alone() {
        let java = "import a.b.C;\nimport static a.b.D.e;\nimport a.*;\nclass X {}\n";
        assert_eq!(sources(java, Lang::Java), ["a.b.C", "a.b.D.e", "a.*"]);
        let kotlin = "import a.b.C\nimport a.b.*\nimport a.b.D as E\n";
        assert_eq!(sources(kotlin, Lang::Kotlin), ["a.b.C", "a.b.*", "a.b.D"]);
    }

    /// A Rust `use` group names one module per member, and each one that is
    /// local resolves to its own file. Read off the line as one source, the
    /// group was `crate` and resolved to nothing.
    #[test]
    fn rust_use_group_resolves_every_member() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(src.join("seams")).unwrap();
        fs::write(src.join("core.rs"), "").unwrap();
        fs::write(src.join("seams/git.rs"), "").unwrap();
        let content = "use crate::{core, seams::git};\nuse std::fs;\n";
        let related = resolve_related_files_with_content(&src.join("main.rs"), content);
        assert_eq!(related, [src.join("core.rs"), src.join("seams/git.rs")]);
    }

    /// An import written across several lines names its module on the last
    /// one. Read off the first line alone, the source was `{`.
    #[test]
    fn multi_line_js_import_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("util.ts"), "").unwrap();
        let content = "import {\n  first,\n  second,\n} from \"./util\";\n";
        let related = resolve_related_files_with_content(&tmp.path().join("main.ts"), content);
        assert_eq!(related, [tmp.path().join("util.ts")]);
    }

    /// PHP, C# and Swift statements come from the tree: a PHP group names
    /// each member under its prefix, and no alias is part of a source. Read
    /// off the line, the group was `App\\Models\\` and the alias stayed on.
    #[test]
    fn php_csharp_and_swift_imports_name_each_module_without_aliases() {
        let php = "<?php\nuse App\\Models\\{User, Post as P};\nuse Foo\\Bar as Baz, Qux;\nuse function Foo\\helper;\n$s = 'use Not\\Real;';\nclass A {}\n";
        assert_eq!(
            sources(php, Lang::Php),
            [
                "App\\Models\\User",
                "App\\Models\\Post",
                "Foo\\Bar",
                "Qux",
                "Foo\\helper"
            ]
        );
        let csharp = "using System;\nusing Json = Newtonsoft.Json;\nusing static System.Math;\nnamespace N {\n  using Inner.Thing;\n  class A {}\n}\n";
        assert_eq!(
            sources(csharp, Lang::CSharp),
            ["System", "Newtonsoft.Json", "System.Math", "Inner.Thing"]
        );
        let swift =
            "import Foundation\nimport struct Foo.Bar\n@testable import MyApp\nclass A {}\n";
        assert_eq!(
            sources(swift, Lang::Swift),
            ["Foundation", "Foo.Bar", "MyApp"]
        );
    }

    /// A re-export imports the module it names. Matching only `import`, a
    /// barrel that re-exports had no imports at all.
    #[test]
    fn js_re_exports_are_imports() {
        let ts = "export { y } from \"./y\";\nexport * from './all';\nexport * as ns from \"./ns\";\nimport { z } from \"./z\";\nexport { local };\nexport const k = 1;\n";
        for lang in [Lang::TypeScript, Lang::Tsx, Lang::JavaScript] {
            assert_eq!(
                sources(ts, lang),
                ["./y", "./all", "./ns", "./z"],
                "{lang:?}"
            );
        }
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("y.ts"), "").unwrap();
        fs::write(tmp.path().join("z.ts"), "").unwrap();
        let related = resolve_related_files_with_content(
            &tmp.path().join("index.ts"),
            "export { y } from \"./y\";\nimport { z } from \"./z\";\n",
        );
        assert_eq!(related, [tmp.path().join("y.ts"), tmp.path().join("z.ts")]);
    }

    #[test]
    fn normalize_collapses_dot_and_parent_components() {
        let p = Path::new("temporal/workflows/../utils/activityProxies.ts");
        assert_eq!(
            normalize_path(p),
            PathBuf::from("temporal/utils/activityProxies.ts")
        );
        let p = Path::new("app/db/./db.ts");
        assert_eq!(normalize_path(p), PathBuf::from("app/db/db.ts"));
    }

    #[test]
    fn normalize_preserves_leading_parent_when_unresolvable() {
        // No prior Normal component to pop, so ".." is kept.
        let p = Path::new("../outside.ts");
        assert_eq!(normalize_path(p), PathBuf::from("../outside.ts"));
    }

    #[test]
    fn js_resolve_returns_normalized_path_for_parent_import() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("temporal/workflows")).unwrap();
        fs::create_dir_all(root.join("temporal/utils")).unwrap();
        fs::write(root.join("temporal/utils/activityProxies.ts"), "").unwrap();

        let resolved = resolve_js(&root.join("temporal/workflows"), "../utils/activityProxies")
            .expect("should resolve");
        let normalized = normalize_path(&resolved);
        assert_eq!(normalized, root.join("temporal/utils/activityProxies.ts"));
        // No "../" component should survive normalization.
        assert!(
            !normalized
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
            "normalized path still contains '..': {normalized:?}"
        );
    }

    #[test]
    fn js_resolve_dedups_different_spellings_of_same_file() {
        // Two importers of the same file via different relative paths must
        // produce the same PathBuf so that hot-file counting aggregates them.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::create_dir_all(root.join("a/c")).unwrap();
        fs::write(root.join("a/b/target.ts"), "").unwrap();

        let from_sibling =
            resolve(&root.join("a/b"), "./target", Lang::TypeScript).expect("sibling");
        let from_cousin =
            resolve(&root.join("a/c"), "../b/target", Lang::TypeScript).expect("cousin");

        assert_eq!(
            from_sibling, from_cousin,
            "different spellings should normalize to the same PathBuf"
        );
    }

    #[test]
    fn bash_is_import_line_tab_separated() {
        // Tab between `source` and path is valid bash and must be detected.
        assert!(
            is_import_line("source\t./lib.sh", Lang::Bash),
            "source<TAB>./lib.sh should be detected as an import line"
        );
        // False positives: `sourcefile=1` looks like it starts with `source` but
        // has no whitespace separator.
        assert!(
            !is_import_line("sourcefile=1", Lang::Bash),
            "sourcefile=1 must not be detected as an import line"
        );
        // `./script.sh` is a script execution, not a source directive.
        assert!(
            !is_import_line("./script.sh", Lang::Bash),
            "./script.sh must not be detected as an import line"
        );
        // `.bashrc` — dot followed by non-whitespace, not a source directive.
        assert!(
            !is_import_line(".bashrc", Lang::Bash),
            ".bashrc must not be detected as an import line"
        );
    }
}
