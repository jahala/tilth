//! Enclosing-scope annotator: given a `(file, line)`, return the nearest
//! enclosing definition, qualified with its containing type or module.
//! Used by the search formatter to annotate usages with their containing
//! function/class/module, and internally by `callers::find_enclosing_function`
//! when reporting the calling-function context of a call site.

use std::path::Path;

use crate::cache::OutlineCache;
use crate::lang::elixir::{extract_elixir_definition_name, is_elixir_definition};
use crate::lang::treesitter::{
    extract_definition_name, is_local_binding, node_text_simple, NodeTextMode, DEFINITION_KINDS,
    TYPE_KINDS,
};
use crate::types::Lang;

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

/// Walk up the AST from `node` to the nearest definition, qualified with the
/// type that owns it (see [`definition_owner`]). Returns the AST node so the
/// caller can read its kind, plus the rendered name and line range.
///
/// A variable local to a function is not a definition (see
/// [`is_local_binding`]), so a node in its initializer belongs to the
/// function: `const result = target()` inside `outer` is in `outer`. A
/// JavaScript function value is a definition when it has a name of its own
/// or is bound to one (see [`function_value_name`]); an anonymous callback is
/// part of the function around it.
pub(super) fn walk_to_enclosing_definition<'a>(
    node: tree_sitter::Node<'a>,
    lines: &[&str],
    lang: Lang,
) -> Option<(tree_sitter::Node<'a>, String, (u32, u32))> {
    let mut current = Some(node);
    while let Some(n) = current {
        let def_name = if matches!(lang, Lang::JavaScript | Lang::TypeScript | Lang::Tsx)
            && FUNCTION_VALUE_KINDS.contains(&n.kind())
        {
            function_value_name(n, lines)
        } else if DEFINITION_KINDS.contains(&n.kind()) && !is_local_binding(n) {
            extract_definition_name(n, lines)
        } else if lang == Lang::Elixir && is_elixir_definition(n, lines) {
            extract_elixir_definition_name(n, lines)
        } else {
            None
        };

        if let Some(name) = def_name {
            let range = (
                n.start_position().row as u32 + 1,
                n.end_position().row as u32 + 1,
            );
            let name = match definition_owner(n, lines, lang) {
                Some(owner) => format!("{owner}.{name}"),
                None => name,
            };
            return Some((n, name, range));
        }
        current = n.parent();
    }
    None
}

/// JavaScript and TypeScript functions written as values.
const FUNCTION_VALUE_KINDS: &[&str] = &[
    "function_expression",
    "function",
    "arrow_function",
    "generator_function",
];

/// The name a function value goes by: its own (`function send() {}`), or
/// the name it is bound to (`const send = () => {}`, `res.send = function
/// () {}`, `{ send() {} }`'s key, a class field). `None` for an anonymous
/// callback.
fn function_value_name(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    let text = |n: tree_sitter::Node| Some(node_text_simple(n, lines, NodeTextMode::Full));
    if let Some(name) = node.child_by_field_name("name") {
        return text(name);
    }
    let parent = node.parent()?;
    let bound = match parent.kind() {
        "variable_declarator" => parent.child_by_field_name("name"),
        "assignment_expression" => parent.child_by_field_name("left").and_then(|left| {
            if left.kind() == "member_expression" {
                left.child_by_field_name("property")
            } else {
                Some(left)
            }
        }),
        "pair" => parent.child_by_field_name("key"),
        "public_field_definition" | "field_definition" => parent
            .child_by_field_name("name")
            .or_else(|| parent.child_by_field_name("property")),
        _ => None,
    }?;
    matches!(
        bound.kind(),
        "identifier" | "property_identifier" | "private_property_identifier"
    )
    .then(|| text(bound))
    .flatten()
}

/// The name of the type or module that owns definition `node`, if any: the
/// nearest enclosing class, struct, trait, interface, module or Rust `impl`
/// (by its type), a Go method's receiver type, or the class a C++ member
/// defined outside it names (`int B::run()`). Elixir's `defmodule` is a
/// `call`, so it is checked apart. This is the owner a qualified name such as
/// `B::run` or `Context.reset` names.
#[must_use]
pub fn definition_owner(node: tree_sitter::Node, lines: &[&str], lang: Lang) -> Option<String> {
    if let Some(owner) = receiver_type(node, lines).or_else(|| declarator_scope(node, lines)) {
        return Some(owner);
    }
    let mut parent = node.parent();
    while let Some(p) = parent {
        if TYPE_KINDS.contains(&p.kind()) {
            let name = if p.kind() == "impl_item" {
                p.child_by_field_name("type")
                    .and_then(|t| base_type_name(t, lines))
            } else {
                extract_definition_name(p, lines)
            };
            if name.is_some() {
                return name;
            }
        }
        if lang == Lang::Elixir && is_elixir_definition(p, lines) {
            if let Some(name) = extract_elixir_definition_name(p, lines) {
                return Some(name);
            }
        }
        parent = p.parent();
    }
    None
}

/// Split a qualified name into its owner and its bare name, at the last `::`
/// or `.`: `B::run` and `B.run` give `(Some("B"), "run")`, and
/// `crate::m::B::run` gives `(Some("B"), "run")` too, since the owner is the
/// type that holds the name. A name without a qualifier, or with an empty
/// segment on either side of the last separator, is returned whole with no
/// owner. Labels from [`walk_to_enclosing_definition`] use `.`, so this also
/// splits them.
#[must_use]
pub fn split_qualified(name: &str) -> (Option<&str>, &str) {
    let Some(cut) = name.rfind(['.', ':']) else {
        return (None, name);
    };
    let bare = &name[cut + 1..];
    let head = if name[..=cut].ends_with("::") {
        &name[..cut - 1]
    } else if name[..=cut].ends_with('.') {
        &name[..cut]
    } else {
        return (None, name);
    };
    let owner = head.rsplit(['.', ':']).next().unwrap_or(head);
    let is_word = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
    };
    if is_word(owner) && is_word(bare) && !head.ends_with(':') {
        (Some(owner), bare)
    } else {
        (None, name)
    }
}

/// A Go method's receiver type: `Context` in `func (c *Context) reset()`.
fn receiver_type(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    if node.kind() != "method_declaration" {
        return None;
    }
    let receiver = node.child_by_field_name("receiver")?;
    let mut cursor = receiver.walk();
    let param = receiver
        .named_children(&mut cursor)
        .find(|c| c.kind() == "parameter_declaration")?;
    base_type_name(param.child_by_field_name("type")?, lines)
}

/// The class a C or C++ function definition names in its declarator, as
/// `B` in `int B::run()` or `int ns::B::run()`.
fn declarator_scope(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    if node.kind() != "function_definition" {
        return None;
    }
    let mut declarator = node.child_by_field_name("declarator");
    while let Some(d) = declarator {
        if d.kind() == "qualified_identifier" {
            let mut qualified = d;
            while let Some(inner) = qualified
                .child_by_field_name("name")
                .filter(|n| n.kind() == "qualified_identifier")
            {
                qualified = inner;
            }
            return base_type_name(qualified.child_by_field_name("scope")?, lines);
        }
        if !d.kind().ends_with("declarator") {
            return None;
        }
        declarator = d.child_by_field_name("declarator");
    }
    None
}

/// The bare name of a type node: `Wrap` for `Wrap<T>`, `Foo` for
/// `crate::a::Foo`, `*Foo` or `&Foo`.
fn base_type_name(node: tree_sitter::Node, lines: &[&str]) -> Option<String> {
    match node.kind() {
        "type_identifier" | "identifier" | "namespace_identifier" | "constant" => {
            Some(node_text_simple(node, lines, NodeTextMode::Full))
        }
        "generic_type" | "reference_type" => base_type_name(
            node.child_by_field_name("type")
                .or_else(|| node.child_by_field_name("name"))
                .or_else(|| node.named_child(0))?,
            lines,
        ),
        "scoped_type_identifier" | "qualified_type" | "template_type" | "scoped_identifier" => {
            base_type_name(node.child_by_field_name("name")?, lines)
        }
        "pointer_type" => {
            let mut cursor = node.walk();
            let pointee = node.named_children(&mut cursor).last()?;
            base_type_name(pointee, lines)
        }
        _ => None,
    }
}

/// Find the nearest enclosing definition for `(path, line)` by re-parsing
/// the file with tree-sitter (cached on `OutlineCache`). AST-correct across
/// every language tilth supports — replaces parsing the rendered outline
/// string back into structured data.
///
/// Returns `None` if the file isn't a code file, the parse fails, or `line`
/// sits at the top level outside any definition.
#[must_use]
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
fn kind_label(node: tree_sitter::Node, lines: &[&str], lang: Lang) -> &'static str {
    match node.kind() {
        "function_declaration"
        | "function_definition"
        | "function_item"
        | "generator_function_declaration"
        | "method_definition"
        | "method_declaration"
        | "method"
        | "singleton_method"
        | "function_signature_item"
        | "method_signature"
        | "abstract_method_signature"
        | "function_signature"
        | "protocol_function_declaration"
        | "decorated_definition"
        | "function_expression"
        | "function"
        | "arrow_function"
        | "generator_function" => "function",
        "class_declaration"
        | "class_definition"
        | "abstract_class_declaration"
        | "class"
        | "record_declaration"
        | "annotation_type_declaration" => "class",
        "struct_item" | "struct_declaration" | "union_item" => "struct",
        "interface_declaration" => "interface",
        "trait_declaration" | "trait_item" | "trait_definition" => "trait",
        "protocol_declaration" => "protocol",
        "type_alias_declaration" | "type_item" | "type_declaration" => "type",
        "enum_item" | "enum_declaration" => "enum",
        "lexical_declaration" | "variable_declaration" | "var_definition" => "variable",
        "const_item" | "const_declaration" | "val_definition" => "const",
        "static_item" => "static",
        "property_declaration" => "property",
        "mod_item"
        | "namespace_definition"
        | "namespace_declaration"
        | "file_scoped_namespace_declaration"
        | "internal_module"
        | "module" => "module",
        "macro_definition" => "macro",
        "object_declaration" | "object_definition" => "object",
        "impl_item" => "impl",
        "export_statement" => "export",
        "call" if lang == Lang::Elixir => elixir_kind_label(node, lines),
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
    match node_text_simple(target, lines, NodeTextMode::Full).as_str() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn split_qualified_takes_the_last_segment_and_its_owner() {
        for (name, owner, bare) in [
            ("Type::method", Some("Type"), "method"),
            ("Type.method", Some("Type"), "method"),
            ("a::b::method", Some("b"), "method"),
            ("a.b::method", Some("b"), "method"),
            ("a::b.method", Some("b"), "method"),
            ("café::méthode", Some("café"), "méthode"),
            ("类型.方法", Some("类型"), "方法"),
            ("method", None, "method"),
            ("メソッド", None, "メソッド"),
            ("Type::", None, "Type::"),
            ("Type.", None, "Type."),
            ("::method", None, "::method"),
            ("a:b", None, "a:b"),
            ("a:::b", None, "a:::b"),
            ("a..b", None, "a..b"),
        ] {
            assert_eq!(split_qualified(name), (owner, bare), "{name}");
        }
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
    fn enclosing_at_rust_impl_block_qualifies_with_its_type() {
        // tree-sitter-rust's `impl_item` names its type in a `type` field, not
        // a `name`. A method inside `impl Foo {...}` is still `Foo.bar`, the
        // way a TypeScript method is `Class.method`, so a label can be passed
        // back to grok as a qualified name.
        let tmp = tempfile::tempdir().unwrap();
        let p = write(
            tmp.path(),
            "a.rs",
            "struct Foo;\nimpl Foo {\n    fn bar(&self) {\n        let x = 1;\n    }\n}\n",
        );
        let cache = OutlineCache::new();
        let scope = enclosing_definition_at(&p, 4, &cache).unwrap();
        assert_eq!(scope.kind, "function");
        assert_eq!(scope.name, "Foo.bar");
    }

    #[test]
    fn enclosing_at_c_function_is_its_bare_name() {
        // Line 12 sits in the body of `int add(int a, int b)`.
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "a.c", crate::lang::c::C_DEFINITIONS_FIXTURE);
        let cache = OutlineCache::new();
        let scope = enclosing_definition_at(&p, 12, &cache).unwrap();
        assert_eq!(scope.kind, "function");
        assert_eq!(scope.name, "add");
    }

    #[test]
    fn enclosing_at_cpp_method_qualifies_with_class() {
        // A method body inside a class, and inside a struct in a namespace,
        // is qualified with its type, the same way a TypeScript method is.
        let tmp = tempfile::tempdir().unwrap();
        let p = write(
            tmp.path(),
            "a.cpp",
            "class Shape {\n  int area() {\n    return 0;\n  }\n};\nnamespace geo {\nstruct Box {\n  int volume() {\n    return 1;\n  }\n};\n}\n",
        );
        let cache = OutlineCache::new();
        for (line, name) in [(3, "Shape.area"), (9, "Box.volume")] {
            let scope = enclosing_definition_at(&p, line, &cache).unwrap();
            assert_eq!(scope.kind, "function", "line {line}");
            assert_eq!(scope.name, name, "line {line}");
        }
    }
}
