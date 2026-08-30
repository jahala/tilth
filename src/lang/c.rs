//! C language spec. Shares its callee query with C++.

use crate::lang::spec::{LangSpec, StdlibRule, StripFamily, DEFAULT_DEFS, DEFAULT_DEF_KINDS};

/// Callee query shared by C and C++.
pub(crate) const CALLEE_QUERY: &str = concat!(
    "(call_expression function: (identifier) @callee)\n",
    "(call_expression function: (field_expression field: (field_identifier) @callee))\n",
);

pub(crate) const SPEC: LangSpec = LangSpec {
    display: "C",
    extensions: &["c", "h"],
    filenames: &[],
    grammar: Some(tree_sitter_c::LANGUAGE),
    callee_query: Some(CALLEE_QUERY),
    sibling_query: None,
    stdlib: StdlibRule::None,
    manifests: &[],
    definition_kinds: DEFAULT_DEF_KINDS,
    has_lifetimes: false,
    strip_family: Some(StripFamily::CppC),
    extract_receiver: None,
    definitions: DEFAULT_DEFS,
};
