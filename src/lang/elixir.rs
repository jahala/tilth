//! Elixir language spec. Diverges on the entire definition cluster: in Elixir
//! every definition is a `call` node whose `target` identifier is the keyword
//! (`def`, `defmodule`, …), so the default field-name extractor does not apply.
//! The definition detection + name extraction + weight live here and are wired
//! into [`SPEC`] via its `definition_kinds` / `definitions` fields.

use crate::lang::spec::{DefinitionOps, LangSpec, StdlibRule};
use crate::lang::treesitter::{
    elixir_arguments, elixir_extract_func_head_name, node_text_simple, NodeTextMode,
};

const CALLEE_QUERY: &str = concat!(
    "(call target: (identifier) @callee)\n",
    "(call target: (dot right: (identifier) @callee))\n",
);

/// Elixir call-node target identifiers that define named symbols.
/// This is the complete set used for definition detection in symbol search/index.
/// See also `ELIXIR_DEF_KEYWORDS` in `outline.rs` which is the subset of
/// function-like keywords (excludes container keywords like `defmodule`,
/// `defprotocol`, `defimpl`, `defstruct`, `defexception` that have their own
/// outline handling).
pub(crate) const ELIXIR_DEFINITION_TARGETS: &[&str] = &[
    "defmodule",
    "def",
    "defp",
    "defmacro",
    "defmacrop",
    "defguard",
    "defguardp",
    "defdelegate",
    "defstruct",
    "defexception",
    "defprotocol",
    "defimpl",
];

pub(crate) const SPEC: LangSpec = LangSpec {
    display: "Elixir",
    extensions: &["ex", "exs"],
    filenames: &[],
    grammar: Some(tree_sitter_elixir::LANGUAGE),
    callee_query: Some(CALLEE_QUERY),
    sibling_query: None,
    stdlib: StdlibRule::None,
    manifests: &["mix.exs"],
    definition_kinds: ELIXIR_DEFINITION_TARGETS,
    has_lifetimes: false,
    strip_family: None,
    extract_receiver: None,
    definitions: DefinitionOps {
        extract_name: extract_elixir_definition_name,
        weight: elixir_definition_weight,
    },
};

/// Check if a tree-sitter node is an Elixir definition.
/// In Elixir all definitions are `call` nodes whose `target` identifier
/// is one of `defmodule`, `def`, `defp`, etc.
pub(crate) fn is_elixir_definition(node: tree_sitter::Node, lines: &[&str]) -> bool {
    if node.kind() != "call" {
        return false;
    }
    let Some(target) = node.child_by_field_name("target") else {
        return false;
    };
    let kw = node_text_simple(target, lines, NodeTextMode::Full);
    ELIXIR_DEFINITION_TARGETS.contains(&kw.as_str())
}

/// Extract the defined name from an Elixir definition `call` node.
///
/// - `defmodule Foo.Bar do...end` → `"Foo.Bar"`
/// - `def greet(name) do...end`  → `"greet"`
/// - `defstruct [:a, :b]`       → `"defstruct"`
pub(crate) fn extract_elixir_definition_name(
    node: tree_sitter::Node,
    lines: &[&str],
) -> Option<String> {
    let target = node.child_by_field_name("target")?;
    let kw = node_text_simple(target, lines, NodeTextMode::Full);
    let args = elixir_arguments(node)?;

    match kw.as_str() {
        "defmodule" | "defprotocol" | "defimpl" => {
            // First named child of arguments is the module/protocol alias.
            // For `defimpl Printable, for: User`, this returns "Printable" (the
            // protocol name), not "User" (the implementing type). Searching for
            // the protocol name will find both the protocol and all its impls.
            let mut cursor = args.walk();
            for child in args.children(&mut cursor) {
                if child.is_named() {
                    return Some(node_text_simple(child, lines, NodeTextMode::Full));
                }
            }
            None
        }
        "def" | "defp" | "defmacro" | "defmacrop" | "defguard" | "defguardp" | "defdelegate" => {
            // First named child is:
            //   `call`              — normal: `def greet(name)`
            //   `identifier`        — no-arg: `def bar, do: :ok`
            //   `binary_operator`   — guard:  `def foo(x) when x > 0`
            let mut cursor = args.walk();
            for child in args.children(&mut cursor) {
                if !child.is_named() {
                    continue;
                }
                return elixir_extract_func_head_name(child, lines);
            }
            None
        }
        // In Elixir, a struct IS its enclosing module (`%MyModule{}`), and only
        // one struct per module is allowed. There's no standalone struct name to
        // extract, so we index the keyword itself. Search for the struct by its
        // module name instead.
        "defstruct" | "defexception" => Some(kw.clone()),
        _ => None,
    }
}

/// Semantic weight for Elixir definition keywords. Matches the `DefinitionOps.weight`
/// signature `(node, lines) -> u16`.
pub(crate) fn elixir_definition_weight(node: tree_sitter::Node, lines: &[&str]) -> u16 {
    let Some(target) = node.child_by_field_name("target") else {
        return 50;
    };
    let kw = node_text_simple(target, lines, NodeTextMode::Full);
    match kw.as_str() {
        "defmodule" | "defprotocol" | "def" | "defp" | "defmacro" | "defmacrop" | "defguard"
        | "defguardp" | "defdelegate" => 100,
        "defimpl" => 90,
        "defstruct" | "defexception" => 80,
        _ => 50,
    }
}
