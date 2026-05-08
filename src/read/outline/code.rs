use crate::lang::outline::{extract_import_source, outline_language, walk_top_level};
use crate::types::{Lang, OutlineEntry, OutlineKind};

/// Generate a code outline using tree-sitter. Walks top-level AST nodes,
/// emitting signatures without bodies.
pub fn outline(content: &str, lang: Lang, max_lines: usize) -> String {
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

    format_entries(&entries, &lines, max_lines, lang)
}

/// Format outline entries into the spec'd output format.
fn format_entries(
    entries: &[OutlineEntry],
    _lines: &[&str],
    max_lines: usize,
    lang: Lang,
) -> String {
    let mut out = Vec::new();
    let mut import_groups: Vec<&str> = Vec::new();
    // Track the start line of the first import in the current group.
    let mut import_group_start: u32 = 1;

    for entry in entries {
        if out.len() >= max_lines {
            break;
        }

        match entry.kind {
            OutlineKind::Import => {
                if import_groups.is_empty() {
                    import_group_start = entry.start_line;
                }
                import_groups.push(&entry.name);
                continue;
            }
            _ => {
                // Flush any accumulated imports
                if !import_groups.is_empty() {
                    out.push(format_imports(&import_groups, import_group_start, lang));
                    import_groups.clear();
                }
            }
        }

        // Flatten namespace modules — hoist their children to top level
        // so classes inside namespaces show their methods at indent 1.
        if entry.kind == OutlineKind::Module && !entry.children.is_empty() {
            out.push(format_entry(entry, 0, lang));
            for child in &entry.children {
                if out.len() >= max_lines {
                    break;
                }
                out.push(format_entry(child, 1, lang));
                for grandchild in &child.children {
                    if out.len() >= max_lines {
                        break;
                    }
                    out.push(format_entry(grandchild, 2, lang));
                }
            }
        } else {
            out.push(format_entry(entry, 0, lang));
            for child in &entry.children {
                if out.len() >= max_lines {
                    break;
                }
                out.push(format_entry(child, 1, lang));
            }
        }
    }

    // Flush trailing imports
    if !import_groups.is_empty() {
        out.push(format_imports(&import_groups, import_group_start, lang));
    }

    out.join("\n")
}

/// Format a collapsed import summary grouped by source with counts.
/// Spec format: `imports: react(4), express(2), @/lib(3)`
fn format_imports(imports: &[&str], start: u32, lang: Lang) -> String {
    let count = imports.len();

    // Extract source modules and count occurrences
    let mut sources: Vec<String> = Vec::new();
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for imp in imports {
        let source = extract_import_source(imp, Some(lang));
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
fn format_entry(entry: &OutlineEntry, indent: usize, lang: Lang) -> String {
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
            if lang == Lang::Scala {
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
        OutlineKind::TestSuite => "suite",
        OutlineKind::TestCase => "test",
    };

    let sig = match &entry.signature {
        Some(s) => format!("\n{prefix}           {s}"),
        None => String::new(),
    };

    let doc = match &entry.doc {
        Some(d) => {
            let truncated = if d.len() > 60 {
                format!("{}...", crate::types::truncate_str(d, 57))
            } else {
                d.clone()
            };
            format!("  // {truncated}")
        }
        None => String::new(),
    };

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

    #[test]
    fn php_outline_constructs() {
        let php_code = r#"<?php
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
"#;

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
        let ts_code = r#"
export async function foo() {}

export class Foo {}

export const x = 1;

export { foo };

export * from './bar';

export default class Bar {}
"#;

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
