//! Swift language spec.

use crate::lang::spec::{LangSpec, StdlibRule, DEFAULT_DEFS, DEFAULT_DEF_KINDS};

const CALLEE_QUERY: &str = concat!(
    "(call_expression (simple_identifier) @callee)\n",
    "(call_expression (navigation_expression suffix: (navigation_suffix suffix: (simple_identifier) @callee)))\n",
);

const SIBLING_QUERY: &str =
    "(navigation_expression target: (self_expression) suffix: (navigation_suffix suffix: (simple_identifier) @ref))\n";

pub(crate) const SPEC: LangSpec = LangSpec {
    display: "Swift",
    extensions: &["swift"],
    filenames: &[],
    grammar: Some(tree_sitter_swift::LANGUAGE),
    callee_query: Some(CALLEE_QUERY),
    sibling_query: Some(SIBLING_QUERY),
    stdlib: StdlibRule::None,
    manifests: &[],
    definition_kinds: DEFAULT_DEF_KINDS,
    has_lifetimes: false,
    strip_family: None,
    extract_receiver: None,
    definitions: DEFAULT_DEFS,
};
