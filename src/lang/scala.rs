//! Scala language spec.

use crate::lang::spec::{LangSpec, StdlibRule, StripFamily, DEFAULT_DEFS, DEFAULT_DEF_KINDS};

const CALLEE_QUERY: &str = concat!(
    "(call_expression function: (identifier) @callee)\n",
    "(call_expression function: (field_expression field: (identifier) @callee))\n",
    "(infix_expression operator: (identifier) @callee)\n",
);

const SIBLING_QUERY: &str = concat!(
    "(field_expression (identifier) @obj (identifier) @ref)\n",
    "(call_expression function: (field_expression (identifier) @obj (identifier) @ref))\n",
);

pub(crate) const SPEC: LangSpec = LangSpec {
    display: "Scala",
    extensions: &["scala", "sc"],
    filenames: &[],
    grammar: Some(tree_sitter_scala::LANGUAGE),
    callee_query: Some(CALLEE_QUERY),
    sibling_query: Some(SIBLING_QUERY),
    stdlib: StdlibRule::None,
    manifests: &["build.sbt"],
    definition_kinds: DEFAULT_DEF_KINDS,
    has_lifetimes: false,
    strip_family: Some(StripFamily::JavaKotlinCSharp),
    extract_receiver: None,
    definitions: DEFAULT_DEFS,
};
