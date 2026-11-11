use tilth_core::imports::{imports_in_tree, Import};
use tilth_core::lang::outline::{extract_import_source, outline_language, walk_top_level};
use tilth_core::types::{Lang, OutlineEntry, OutlineKind};

/// What each outline entry shows besides its line range, kind and name.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// The signature and doc comment too.
    Full,
    /// The name alone, one line per entry: the outline that fits a tight
    /// budget with every symbol still on it.
    Names,
}

/// Generate a code outline using tree-sitter. Walks top-level AST nodes,
/// emitting signatures without bodies.
#[must_use]
pub fn outline(content: &str, lang: Lang, max_lines: usize) -> String {
    outline_with(content, lang, max_lines, Detail::Full)
}

/// [`outline`] with the entries' `detail`.
#[must_use]
pub fn outline_with(content: &str, lang: Lang, max_lines: usize, detail: Detail) -> String {
    let Some(language) = outline_language(lang) else {
        return fallback_outline(content, max_lines);
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language).is_err() {
        return fallback_outline(content, max_lines);
    }

    let Some(tree) = parser.parse(content, None) else {
        return fallback_outline(content, max_lines);
    };

    let root = tree.root_node();
    let lines: Vec<&str> = content.lines().collect();
    let entries = walk_top_level(root, &lines, lang);
    let imports = imports_in_tree(root, content, lang);

    format_entries(&entries, &imports, max_lines, lang, detail)
}

/// Format outline entries into the spec'd output format.
fn format_entries(
    entries: &[OutlineEntry],
    imports: &[Import],
    max_lines: usize,
    lang: Lang,
    detail: Detail,
) -> String {
    let mut out = Vec::new();
    let mut import_groups: Vec<&OutlineEntry> = Vec::new();
    // Track the start line of the first import in the current group.
    let mut import_group_start: u32 = 1;
    // The export clause last rendered: its specifiers' entries share it, and
    // the clause renders once.
    let mut last_clause: Option<(u32, u32, &str)> = None;

    for entry in entries {
        if out.len() >= max_lines {
            break;
        }
        if entry.kind == OutlineKind::Export {
            if let Some(clause) = entry.signature.as_deref() {
                let key = (entry.start_line, entry.end_line, clause);
                if last_clause == Some(key) {
                    continue;
                }
                last_clause = Some(key);
            }
        }

        match entry.kind {
            OutlineKind::Import => {
                if import_groups.is_empty() {
                    import_group_start = entry.start_line;
                }
                import_groups.push(entry);
                continue;
            }
            _ => {
                // Flush any accumulated imports
                if !import_groups.is_empty() {
                    out.push(format_imports(
                        &import_groups,
                        imports,
                        import_group_start,
                        lang,
                    ));
                    import_groups.clear();
                }
            }
        }

        // Flatten namespace modules — hoist their children to top level
        // so classes inside namespaces show their methods at indent 1.
        if entry.kind == OutlineKind::Module && !entry.children.is_empty() {
            out.push(format_entry(entry, 0, lang, detail));
            for child in &entry.children {
                if out.len() >= max_lines {
                    break;
                }
                out.push(format_entry(child, 1, lang, detail));
                for grandchild in &child.children {
                    if out.len() >= max_lines {
                        break;
                    }
                    out.push(format_entry(grandchild, 2, lang, detail));
                }
            }
        } else {
            out.push(format_entry(entry, 0, lang, detail));
            for child in &entry.children {
                if out.len() >= max_lines {
                    break;
                }
                out.push(format_entry(child, 1, lang, detail));
            }
        }
    }

    // Flush trailing imports
    if !import_groups.is_empty() {
        out.push(format_imports(
            &import_groups,
            imports,
            import_group_start,
            lang,
        ));
    }

    out.join("\n")
}

/// Format a collapsed import summary grouped by source with counts.
/// Spec format: `imports: react(4), express(2), @/lib(3)`
///
/// Each module a statement names counts once, so a Go block or a Rust group
/// lists its members. A statement with no module in `imports` (PHP, C# and
/// the other languages `extract_imports` does not read) counts once, by the
/// source its own text names.
fn format_imports(
    statements: &[&OutlineEntry],
    imports: &[Import],
    start: u32,
    lang: Lang,
) -> String {
    let mut named: Vec<String> = Vec::new();
    for statement in statements {
        let before = named.len();
        named.extend(
            imports
                .iter()
                .filter(|i| (statement.start_line..=statement.end_line).contains(&i.start_line))
                .map(|i| i.source.clone()),
        );
        if named.len() == before {
            named.push(extract_import_source(&statement.name, Some(lang)));
        }
    }
    let count = named.len();

    // Count occurrences of each source, in first-seen order
    let mut sources: Vec<String> = Vec::new();
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for source in named {
        *seen.entry(source.clone()).or_insert(0) += 1;
        if !sources.contains(&source) {
            sources.push(source);
        }
    }

    // Format as "source(count)" or just "source" if count is 1
    let mut parts: Vec<String> = Vec::new();
    for src in sources.iter().take(5) {
        let c = seen[src];
        if c > 1 {
            parts.push(format!("{src}({c})"));
        } else {
            parts.push(src.clone());
        }
    }

    let suffix = if count > 5 {
        format!(", ... ({count} total)")
    } else {
        String::new()
    };
    let condensed = parts.join(", ");
    format!("[{start}-]   imports: {condensed}{suffix}")
}

/// Format a single outline entry with optional indentation.
fn format_entry(entry: &OutlineEntry, indent: usize, lang: Lang, detail: Detail) -> String {
    let prefix = "  ".repeat(indent);
    let range = if entry.start_line == entry.end_line {
        format!("[{}]", entry.start_line)
    } else {
        format!("[{}-{}]", entry.start_line, entry.end_line)
    };

    let kind_label = match entry.kind {
        OutlineKind::Function => {
            if lang == Lang::Scala {
                "def"
            } else if lang == Lang::Kotlin {
                "fun"
            } else {
                "fn"
            }
        }
        OutlineKind::Class => "class",
        OutlineKind::Struct => "struct",
        OutlineKind::Interface => {
            if lang == Lang::Scala {
                "trait"
            } else {
                "interface"
            }
        }
        OutlineKind::TypeAlias => "type",
        OutlineKind::Enum => "enum",
        OutlineKind::Constant => "const",
        OutlineKind::ImmutableVariable => "val",
        OutlineKind::Variable => {
            if lang == Lang::Scala || lang == Lang::Go {
                "var"
            } else {
                "let"
            }
        }
        OutlineKind::Export => "export",
        OutlineKind::Property => "prop",
        OutlineKind::Module => {
            if lang == Lang::Scala || lang == Lang::Kotlin {
                "object"
            } else {
                "mod"
            }
        }
        OutlineKind::Import => "import",
    };

    // An export clause renders as written, once for all its specifiers.
    if let (OutlineKind::Export, Some(clause)) = (entry.kind, &entry.signature) {
        let rest = clause.strip_prefix("export ").unwrap_or(clause);
        return format!("{prefix}{range:<12} export {rest}");
    }

    if detail == Detail::Names {
        return format!("{prefix}{range:<12} {kind_label} {}", entry.name);
    }

    let sig = entry
        .signature
        .as_ref()
        .map_or_else(String::new, |s| format!("\n{prefix}           {s}"));

    let doc = entry.doc.as_ref().map_or_else(String::new, |d| {
        let truncated = if d.len() > 60 {
            format!("{}...", tilth_core::types::truncate_str(d, 57))
        } else {
            d.clone()
        };
        format!("  // {truncated}")
    });

    format!("{prefix}{range:<12} {kind_label} {}{sig}{doc}", entry.name)
}

/// Fallback when tree-sitter grammar isn't available.
fn fallback_outline(content: &str, _max_lines: usize) -> String {
    super::fallback::head_tail(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scala_outline_constructs() {
        let scala_code = r#"
package example

import scala.util.Try

trait DataSource {
  def load(): String
}

class Database {
  val connectionString = "jdbc:..."
  var connected = false
  
  def connect(): Unit = {}
}

object Database {
  def create(): Database = new Database()
}

enum Color {
  case Red, Green, Blue
}

type UserId = String
"#;

        let outline = outline(scala_code, Lang::Scala, 1000);

        assert!(outline.contains("trait DataSource"));
        assert!(outline.contains("class Database"));
        assert!(outline.contains("object Database"));
        assert!(outline.contains("enum Color"));
        assert!(outline.contains("type UserId"));
        assert!(outline.contains("val connectionString"));
        assert!(outline.contains("var connected"));
        assert!(outline.contains("def load"));
        assert!(outline.contains("def connect"));
        assert!(outline.contains("def create"));
    }

    /// The import summary names each module a statement imports. Read off
    /// the statement's first line, a Go block was `(` and a Rust group
    /// written across lines was its bare prefix.
    #[test]
    fn import_summary_names_each_module_of_a_statement() {
        let go = "package m\n\nimport (\n\t\"fmt\"\n\tx \"example.com/m/core\"\n)\n\nfunc A() {}\n";
        let outline_go = outline(go, Lang::Go, 1000);
        assert!(
            outline_go.contains("[3-]   imports: fmt, example.com/m/core\n"),
            "{outline_go}"
        );

        let rust = "use std::{\n    fs,\n    io,\n};\nuse crate::a;\n\nfn f() {}\n";
        let outline_rust = outline(rust, Lang::Rust, 1000);
        assert!(
            outline_rust.contains("[1-]   imports: std::fs, std::io, crate::a\n"),
            "{outline_rust}"
        );

        let python = "import os, json\nimport a.b as c\n\ndef f():\n    pass\n";
        let outline_python = outline(python, Lang::Python, 1000);
        assert!(
            outline_python.contains("[1-]   imports: os, json, a.b\n"),
            "{outline_python}"
        );
    }

    /// A Python `from` import is an import: the summary names its module.
    /// The outline gave the statement no entry, so the summary skipped it.
    #[test]
    fn import_summary_names_python_from_imports() {
        let python = "import os\nfrom collections import OrderedDict\nfrom .local import helper\nfrom __future__ import annotations\n\ndef f():\n    pass\n";
        let rendered = outline(python, Lang::Python, 1000);
        assert!(
            rendered.contains("[1-]   imports: os, collections, .local, __future__\n"),
            "{rendered}"
        );
    }

    /// PHP, C# and Swift summaries name each module a statement imports,
    /// without its alias. Read off the statement's text, a PHP group was its
    /// bare prefix and an alias stayed on the source.
    #[test]
    fn import_summary_reads_php_csharp_and_swift_from_the_tree() {
        let php = "<?php\nuse App\\Models\\{User, Post};\nuse Foo\\Bar as Baz;\n\nclass A {}\n";
        let rendered = outline(php, Lang::Php, 1000);
        assert!(
            rendered.contains("imports: App\\Models\\User, App\\Models\\Post, Foo\\Bar\n"),
            "{rendered}"
        );
        let csharp = "using System;\nusing Json = Newtonsoft.Json;\n\nclass A {}\n";
        let rendered = outline(csharp, Lang::CSharp, 1000);
        assert!(
            rendered.contains("imports: System, Newtonsoft.Json\n"),
            "{rendered}"
        );
        let swift = "import Foundation\nimport struct Foo.Bar\n\nclass A {}\n";
        let rendered = outline(swift, Lang::Swift, 1000);
        assert!(
            rendered.contains("imports: Foundation, Foo.Bar\n"),
            "{rendered}"
        );
    }

    /// An export clause renders as one line, the clause as written. Read off
    /// its first line, a clause written across lines was `export {`.
    #[test]
    fn export_clause_renders_once_as_written() {
        let ts = "function readPlan() {}\nexport { readPlan as parsePlan, readNode };\nexport {\n  a,\n  b as c,\n} from \"./y\";\n";
        for lang in [Lang::TypeScript, Lang::JavaScript] {
            let rendered = outline(ts, lang, 1000);
            let lines: Vec<&str> = rendered.lines().collect();
            assert_eq!(
                lines[2..],
                [
                    "[2]          export { readPlan as parsePlan, readNode };",
                    "[3-6]        export { a, b as c, } from \"./y\";",
                ],
                "{lang:?}:\n{rendered}"
            );
        }
    }

    #[test]
    fn php_outline_constructs() {
        let php_code = r"<?php
namespace App\Services;

use App\Support\Client;

trait LogsQueries {
    public function log(string $query): void {}
}

class UserService {
    use LogsQueries;

    public function __construct(private Client $client) {}

    public function findUser(int $id): array {
        return $this->client->loadUser($id);
    }
}
";

        let outline = outline(php_code, Lang::Php, 1000);

        assert!(outline.contains("mod App\\Services"));
        assert!(outline.contains("imports: App\\Support\\Client"));
        assert!(outline.contains("interface LogsQueries"));
        assert!(outline.contains("class UserService"));
        assert!(outline.contains("fn findUser"));
    }

    #[test]
    fn kotlin_outline_constructs() {
        let kotlin_code = r#"
package com.example

import kotlin.collections.List
import kotlin.io.println

interface Drawable {
    fun draw()
}

data class Point(val x: Int, val y: Int)

class Canvas : Drawable {
    val width = 800
    var height = 600

    override fun draw() {
        println("Drawing")
    }

    fun resize(w: Int, h: Int) {}

    companion object {
        fun create(): Canvas = Canvas()
    }
}

object Registry {
    fun register(item: Drawable) {}
}

enum class Color {
    RED, GREEN, BLUE
}

fun String.isPalindrome(): Boolean = this == this.reversed()

fun main() {
    val canvas = Canvas()
    canvas.draw()
}
"#;

        let outline = outline(kotlin_code, Lang::Kotlin, 1000);

        // Imports
        assert!(
            outline.contains("imports:"),
            "should have collapsed imports"
        );
        // Interface (shown as class since Kotlin grammar uses class_declaration)
        assert!(outline.contains("class Drawable"), "should have Drawable");
        // Data class
        assert!(outline.contains("class Point"), "should have Point");
        // Regular class with methods
        assert!(outline.contains("class Canvas"), "should have Canvas");
        assert!(outline.contains("fun draw"), "should have draw method");
        assert!(outline.contains("fun resize"), "should have resize method");
        // Properties inside classes
        assert!(outline.contains("prop width"), "should have width property");
        assert!(
            outline.contains("prop height"),
            "should have height property"
        );
        // Object declaration
        assert!(
            outline.contains("object Registry"),
            "should have Registry object"
        );
        assert!(
            outline.contains("fun register"),
            "should have register method"
        );
        // Enum class
        assert!(outline.contains("class Color"), "should have Color enum");
        // Top-level functions
        assert!(
            outline.contains("fun isPalindrome"),
            "should have extension fun"
        );
        assert!(outline.contains("fun main"), "should have main");
        // Kotlin-specific labels
        assert!(outline.contains("fun "), "should use 'fun' not 'fn'");
        assert!(!outline.contains("fn "), "should not use 'fn' for Kotlin");
    }

    #[test]
    fn ts_export_outline_no_doubled_keyword() {
        // Regression: `export_statement` must recurse into the wrapped
        // declaration (function/class/lexical) and render with that
        // declaration's real kind label. Pre-fix, both the kind label and
        // the synthesized name began with `export `, so the renderer
        // emitted `export export async function foo(`. Leaf-fallback cases
        // (`export { ... }`, `export * from ...`) keep `OutlineKind::Export`
        // but strip the leading `export ` from the name to avoid duplication.
        let ts_code = r"
export async function foo() {}

export class Foo {}

export const x = 1;

export { foo };

export * from './bar';

export default class Bar {}
";

        let outline = outline(ts_code, Lang::TypeScript, 1000);

        // No outline line may contain a doubled `export` keyword.
        for line in outline.lines() {
            assert!(
                !line.contains("export export"),
                "doubled `export` in outline line: {line:?}\nfull outline:\n{outline}"
            );
        }

        // Wrapped declarations must render with the real kind, not as
        // `OutlineKind::Export` carrying the entire source as `name`.
        assert!(
            outline.contains("fn foo"),
            "export async function foo should render as `fn foo`:\n{outline}"
        );
        assert!(
            outline.contains("class Foo"),
            "export class Foo should render as `class Foo`:\n{outline}"
        );
        assert!(
            outline.contains("class Bar"),
            "export default class Bar should render as `class Bar`:\n{outline}"
        );
    }
}
