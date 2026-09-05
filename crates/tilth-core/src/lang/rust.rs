//! Rust language spec. Diverges on: stdlib prefix rule, lifetime ticks.

use crate::lang::spec::{LangSpec, StdlibRule, StripFamily, DEFAULT_DEFS, DEFAULT_DEF_KINDS};

const CALLEE_QUERY: &str = concat!(
    "(call_expression function: (identifier) @callee)\n",
    "(call_expression function: (field_expression field: (field_identifier) @callee))\n",
    "(call_expression function: (scoped_identifier name: (identifier) @callee))\n",
    "(macro_invocation macro: (identifier) @callee)\n",
);

const SIBLING_QUERY: &str = concat!(
    "(field_expression value: (self) field: (field_identifier) @ref)\n",
    "(call_expression function: (field_expression value: (self) field: (field_identifier) @ref))\n",
);

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
    definitions: DEFAULT_DEFS,
};
