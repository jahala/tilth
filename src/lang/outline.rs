use crate::types::{Lang, OutlineEntry, OutlineKind};

/// Get the tree-sitter Language for a given Lang variant.
pub fn outline_language(lang: Lang) -> Option<tree_sitter::Language> {
    let lang = match lang {
        Lang::Rust => tree_sitter_rust::LANGUAGE,
        Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX,
        Lang::JavaScript => tree_sitter_javascript::LANGUAGE,
        Lang::Python => tree_sitter_python::LANGUAGE,
        Lang::Scala => tree_sitter_scala::LANGUAGE,
        Lang::Go => tree_sitter_go::LANGUAGE,
        Lang::Java => tree_sitter_java::LANGUAGE,
        Lang::C => tree_sitter_c::LANGUAGE,
        Lang::Cpp => tree_sitter_cpp::LANGUAGE,
        Lang::Ruby => tree_sitter_ruby::LANGUAGE,
        Lang::Php => tree_sitter_php::LANGUAGE_PHP,
        // Languages without shipped grammars — fall back
        Lang::CSharp => tree_sitter_c_sharp::LANGUAGE,
        Lang::Swift => tree_sitter_swift::LANGUAGE,
        Lang::Kotlin => tree_sitter_kotlin_ng::LANGUAGE,
        Lang::Dockerfile | Lang::Make => {
            return None;
        }
    };
    Some(lang.into())
}

/// Walk top-level children of the root node, extracting outline entries.
pub(crate) fn walk_top_level(
    root: tree_sitter::Node,
    lines: &[&str],
    lang: Lang,
) -> Vec<OutlineEntry> {
    let mut entries = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        if let Some(entry) = node_to_entry(child, lines, lang, 0) {
            entries.push(entry);
        }
    }

    entries
}

/// Convert a tree-sitter node to an `OutlineEntry` based on its kind.
fn node_to_entry(
    node: tree_sitter::Node,
    lines: &[&str],
    lang: Lang,
    depth: usize,
) -> Option<OutlineEntry> {
    let kind_str = node.kind();
    let start_line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;

    let (kind, name, signature) = match kind_str {
        // Functions
        "function_declaration"
        | "function_definition"
        | "function_item"
        | "method_definition"
        | "method_declaration"
        | "constructor_declaration"
        | "init_declaration"
        | "deinit_declaration"
        | "protocol_function_declaration" => {
            let name = find_child_text(node, "name", lines)
                .or_else(|| find_child_text(node, "identifier", lines))
                .unwrap_or_else(|| {
                    // Swift deinit has no name field — use the node kind as name
                    if kind_str == "deinit_declaration" {
                        "deinit".into()
                    } else {
                        "<anonymous>".into()
                    }
                });
            let sig = extract_signature(node, lines);
            (OutlineKind::Function, name, Some(sig))
        }

        // Classes & structs
        "class_declaration" | "class_definition" => {
            let name = find_child_text(node, "name", lines)
                .or_else(|| find_child_text(node, "identifier", lines))
                .unwrap_or_else(|| "<anonymous>".into());
            (OutlineKind::Class, name, None)
        }
        "struct_item" | "struct_declaration" => {
            let name = find_child_text(node, "name", lines).unwrap_or_else(|| "<anonymous>".into());
            (OutlineKind::Struct, name, None)
        }

        // Interfaces & traits
        "interface_declaration"
        | "type_alias_declaration"
        | "trait_item"
        | "trait_declaration"
        | "trait_definition"
        | "protocol_declaration" => {
            let name = find_child_text(node, "name", lines).unwrap_or_else(|| "<anonymous>".into());
            (OutlineKind::Interface, name, None)
        }
        "type_item" | "type_definition" | "typealias_declaration" => {
            let name = find_child_text(node, "name", lines).unwrap_or_else(|| "<anonymous>".into());
            (OutlineKind::TypeAlias, name, None)
        }

        // Enums
        "enum_item" | "enum_declaration" | "enum_definition" => {
            let name = find_child_text(node, "name", lines).unwrap_or_else(|| "<anonymous>".into());
            (OutlineKind::Enum, name, None)
        }

        // Impl blocks (Rust)
        "impl_item" => {
            let name = find_child_text(node, "type", lines).unwrap_or_else(|| "<impl>".into());
            (OutlineKind::Module, format!("impl {name}"), None)
        }

        // Objects (Scala companion objects, singletons; Kotlin object declarations)
        "object_declaration" | "object_definition" => {
            let name = find_child_text(node, "name", lines)
                .or_else(|| find_child_text(node, "identifier", lines))
                .unwrap_or_else(|| "<anonymous>".into());
            (OutlineKind::Module, name, None)
        }

        // Constants and variables
        "const_item" | "const_declaration" | "static_item" => {
            let name = find_child_text(node, "name", lines)
                .or_else(|| first_identifier_text(node, lines))
                .unwrap_or_else(|| "<const>".into());
            (OutlineKind::Constant, name, None)
        }
        "val_definition" => {
            let name = first_identifier_text(node, lines).unwrap_or_else(|| "<val>".into());
            (OutlineKind::ImmutableVariable, name, None)
        }
        "lexical_declaration" | "variable_declaration" | "var_definition" => {
            let name = first_identifier_text(node, lines).unwrap_or_else(|| "<var>".into());
            (OutlineKind::Variable, name, None)
        }

        // Properties (C#, Swift, Kotlin)
        "property_declaration" | "protocol_property_declaration" => {
            let name = find_child_text(node, "name", lines)
                .or_else(|| first_identifier_text(node, lines))
                .unwrap_or_else(|| "<property>".into());
            let sig = extract_signature(node, lines);
            (OutlineKind::Property, name, Some(sig))
        }

        // Imports — collect as a group
        "import_statement"
        | "import_declaration"
        | "import"
        | "use_declaration"
        | "namespace_use_declaration"
        | "use_item"
        | "using_directive" => {
            let text = node_text(node, lines);
            (OutlineKind::Import, text, None)
        }

        // Exports
        "export_statement" => {
            let name = node_text(node, lines);
            (OutlineKind::Export, name, None)
        }

        // Module declarations
        "mod_item"
        | "module"
        | "namespace_declaration"
        | "namespace_definition"
        | "file_scoped_namespace_declaration" => {
            let name = find_child_text(node, "name", lines).unwrap_or_else(|| "<module>".into());
            (OutlineKind::Module, name, None)
        }

        _ => return None,
    };

    // Collect children for classes, impls, modules, traits/interfaces
    let is_namespace = matches!(
        kind_str,
        "namespace_declaration" | "namespace_definition" | "file_scoped_namespace_declaration"
    );
    let children = if matches!(
        kind,
        OutlineKind::Class | OutlineKind::Struct | OutlineKind::Module | OutlineKind::Interface
    ) && depth < 1
    {
        // Namespaces are transparent wrappers — don't consume a depth level,
        // so classes inside namespaces still collect their methods.
        let child_depth = if is_namespace { depth } else { depth + 1 };
        collect_children(node, lines, lang, child_depth)
    } else {
        Vec::new()
    };

    // Extract doc comment if present
    let doc = extract_doc(node, lines);

    Some(OutlineEntry {
        kind,
        name,
        start_line,
        end_line,
        signature,
        children,
        doc,
    })
}

/// Collect child entries from a class/struct/impl body.
fn collect_children(
    node: tree_sitter::Node,
    lines: &[&str],
    lang: Lang,
    depth: usize,
) -> Vec<OutlineEntry> {
    let mut children = Vec::new();
    let mut cursor = node.walk();

    // Look for a body node first (C# uses `declaration_list` instead of `*_body`/`*_block`)
    let body = node.children(&mut cursor).find(|c| {
        let k = c.kind();
        k.contains("body") || k.contains("block") || k == "declaration_list"
    });

    let parent = body.unwrap_or(node);
    let mut cursor2 = parent.walk();

    for child in parent.children(&mut cursor2) {
        if let Some(entry) = node_to_entry(child, lines, lang, depth) {
            children.push(entry);
        }
    }

    children
}

/// Extract the first line as a function signature (name + params + return type).
fn extract_signature(node: tree_sitter::Node, lines: &[&str]) -> String {
    let start_row = node.start_position().row;
    if start_row < lines.len() {
        let line = lines[start_row].trim();
        // Truncate at opening brace
        if let Some(pos) = line.find('{') {
            return line[..pos].trim().to_string();
        }
        if line.ends_with(':') {
            // Python — truncate at trailing colon (for `def foo(x: int):` etc.)
            if let Some(pos) = line.rfind(':') {
                return line[..pos].trim().to_string();
            }
        }
        // Full first line, truncated
        if line.len() > 120 {
            format!("{}...", crate::types::truncate_str(line, 117))
        } else {
            line.to_string()
        }
    } else {
        String::new()
    }
}

/// Find a named child and return its text.
fn find_child_text(node: tree_sitter::Node, field: &str, lines: &[&str]) -> Option<String> {
    node.child_by_field_name(field).map(|n| node_text(n, lines))
}

/// Get the text of a node, truncated to the first line.
fn node_text(node: tree_sitter::Node, lines: &[&str]) -> String {
    let row = node.start_position().row;
    let col_start = node.start_position().column;
    let end_row = node.end_position().row;

    if row < lines.len() {
        if row == end_row {
            let col_end = node.end_position().column.min(lines[row].len());
            lines[row][col_start..col_end].to_string()
        } else {
            // Multi-line — take first line only, truncated
            let text = &lines[row][col_start..];
            if text.len() > 80 {
                format!("{}...", crate::types::truncate_str(text, 77))
            } else {
                text.to_string()
            }
        }
    } else {
        String::new()
    }
}

/// Find the first identifier-like child.
/// Recurses one level through declarators and `variable_declaration` nodes to find
/// the actual identifier inside wrapper nodes (e.g. Kotlin `property_declaration`
/// → `variable_declaration` → `simple_identifier`).
fn first_identifier_text(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();
        if kind.contains("identifier") || kind.contains("name") {
            let text = node_text(child, lines);
            if !text.is_empty() {
                return Some(text);
            }
        }
        // Recurse one level through wrapper nodes (variable_declarator, variable_declaration)
        if kind.contains("declarator") || kind.contains("declaration") {
            let mut inner = child.walk();
            for grandchild in child.children(&mut inner) {
                if grandchild.kind().contains("identifier") {
                    let text = node_text(grandchild, lines);
                    if !text.is_empty() {
                        return Some(text);
                    }
                }
            }
        }
    }
    None
}

/// Extract a doc comment from the previous sibling.
fn extract_doc(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    let prev = node.prev_sibling()?;
    let kind = prev.kind();
    if kind.contains("comment") || kind.contains("doc") {
        let text = node_text(prev, lines);
        let trimmed = text
            .trim_start_matches("///")
            .trim_start_matches("//!")
            .trim_start_matches("/**")
            .trim_start_matches('#')
            .trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    } else {
        None
    }
}

/// Extract the source module name from an import statement text.
/// Handles: `use std::fs;` → `std::fs`, `import X from "react"` → `react`,
/// `from collections import X` → `collections`
pub(crate) fn extract_import_source(text: &str) -> String {
    let trimmed = text.trim().trim_end_matches(';');

    // Rust: `use foo::bar` → `foo::bar`
    if let Some(rest) = trimmed.strip_prefix("use ") {
        return rest
            .split('{')
            .next()
            .unwrap_or(rest)
            .trim()
            .trim_end_matches("::")
            .to_string();
    }

    // JS/TS: `import ... from "source"` or `import "source"`
    if trimmed.starts_with("import") {
        if let Some(from_pos) = trimmed.find("from ") {
            let source = &trimmed[from_pos + 5..];
            return source
                .trim()
                .trim_matches(|c| c == '"' || c == '\'' || c == ';')
                .to_string();
        }
        // Direct import: `import "source"`
        let after = trimmed.strip_prefix("import ").unwrap_or("");
        return after
            .trim()
            .trim_matches(|c| c == '"' || c == '\'' || c == ';')
            .to_string();
    }

    // Python: `from module import ...` or `import module`
    if let Some(rest) = trimmed.strip_prefix("from ") {
        return rest.split_whitespace().next().unwrap_or("").to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("import ") {
        return rest.split_whitespace().next().unwrap_or("").to_string();
    }

    // C/C++: #include "file.h" or #include <header>
    if let Some(rest) = trimmed.strip_prefix("#include") {
        return rest.trim().to_string(); // preserves quotes/angles for external detection
    }

    // Go: `import "source"` — already handled above via "import"
    // Fallback: first meaningful token
    trimmed
        .split_whitespace()
        .last()
        .unwrap_or(trimmed)
        .to_string()
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
    walk_top_level(tree.root_node(), &lines, lang)
}
