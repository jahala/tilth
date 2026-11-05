//! Rust language spec. Diverges on: stdlib prefix rule, lifetime ticks,
//! callee filtering, test-site detection.

use std::path::Path;

use crate::lang::spec::{LangSpec, StdlibRule, StripFamily, DEFAULT_DEFS, DEFAULT_DEF_KINDS};

const CALLEE_QUERY: &str = concat!(
    "(call_expression function: (identifier) @callee)\n",
    "(call_expression function: (field_expression field: (field_identifier) @callee))\n",
    "(call_expression function: (scoped_identifier name: (identifier) @callee))\n",
    "(macro_invocation macro: (identifier) @callee)\n",
    // A macro's arguments are a `token_tree` of raw tokens, so a call written
    // inside `write!(..)` has no `call_expression` node. Requiring the
    // following tree to open with `(` keeps `x[i]` and `Foo { .. }` out; the
    // rest is `callee_filter`'s job.
    "(token_tree (identifier) @callee . (token_tree . \"(\"))\n",
);

const SIBLING_QUERY: &str = concat!(
    "(field_expression value: (self) field: (field_identifier) @ref)\n",
    "(call_expression function: (field_expression value: (self) field: (field_identifier) @ref))\n",
);

/// Drop token-tree captures that the loose half of the query picked up. The
/// `.` between the identifier and the tree skips anonymous nodes, so the
/// pattern also matches the `x` in `assert_eq!(x, (a, b))`; requiring the tree
/// to be the *immediate* next sibling rejects it. Captures from the other four
/// patterns hang off `call_expression` / `field_expression` /
/// `scoped_identifier` / `macro_invocation`, never a token tree, so they pass.
///
/// Raw tokens are all this can go on, so it neither over- nor under-matches
/// perfectly: `matches!(x, Foo(_))` and `#[cfg(not(unix))]` still look like
/// calls, and a turbofish (`m!(parse::<u32>(s))`) still looks like none.
fn callee_is_call(node: &tree_sitter::Node) -> bool {
    if node.parent().is_some_and(|p| p.kind() == "token_tree") {
        node.next_sibling()
            .is_some_and(|n| n.kind() == "token_tree")
    } else {
        true
    }
}

/// Whether a Rust call site is test code. Rust says so in the source — a
/// `#[test]` or `#[<path>::test]` function, a `#[cfg(test)]` module — or by
/// putting the file under a crate's `tests` directory, and none of that is
/// visible to `is_test_file`, which knows only the `.test.` / `.spec.` /
/// `__tests__/` filename conventions Rust never uses. `scope` is the search
/// root the path is judged against, so a checkout that happens to sit under
/// some `tests/` is not read as all tests.
fn rust_test_site(node: &tree_sitter::Node, content: &[u8], path: &Path, scope: &Path) -> bool {
    let relative = path.strip_prefix(scope).unwrap_or(path);
    if relative.components().any(|c| c.as_os_str() == "tests") {
        return true;
    }
    let mut current = Some(*node);
    while let Some(n) = current {
        if matches!(n.kind(), "function_item" | "mod_item") && has_test_attribute(n, content) {
            return true;
        }
        current = n.parent();
    }
    false
}

/// The attributes above a Rust item — its preceding siblings in
/// tree-sitter-rust — include `#[test]`, a `#[<path>::test]` such as
/// `#[tokio::test]`, or `#[cfg(test)]`.
fn has_test_attribute(item: tree_sitter::Node, content: &[u8]) -> bool {
    let mut sibling = item.prev_sibling();
    while let Some(s) = sibling {
        match s.kind() {
            "attribute_item" => {
                let text = s.utf8_text(content).unwrap_or_default();
                let inner: String = text
                    .trim_start_matches("#[")
                    .trim_end_matches(']')
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                let attr_path = inner.split('(').next().unwrap_or_default();
                if attr_path == "test" || attr_path.ends_with("::test") || inner == "cfg(test)" {
                    return true;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => return false,
        }
        sibling = s.prev_sibling();
    }
    false
}

pub(crate) const SPEC: LangSpec = LangSpec {
    display: "Rust",
    extensions: &["rs"],
    filenames: &[],
    grammar: Some(tree_sitter_rust::LANGUAGE),
    callee_query: Some(CALLEE_QUERY),
    sibling_query: Some(SIBLING_QUERY),
    stdlib: StdlibRule::Prefixes(&["std::", "core::", "alloc::"]),
    manifests: &["Cargo.toml"],
    definition_kinds: DEFAULT_DEF_KINDS,
    has_lifetimes: true,
    strip_family: Some(StripFamily::Rust),
    extract_receiver: None,
    callee_filter: Some(callee_is_call),
    test_site: Some(rust_test_site),
    definitions: DEFAULT_DEFS,
};
