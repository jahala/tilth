//! Kotlin language spec.

use crate::lang::spec::{LangSpec, StdlibRule, StripFamily, DEFAULT_DEFS, DEFAULT_DEF_KINDS};

const CALLEE_QUERY: &str = concat!(
    "(call_expression (identifier) @callee)\n",
    "(call_expression (navigation_expression (identifier) @callee .))\n",
);

pub(crate) const SPEC: LangSpec = LangSpec {
    display: "Kotlin",
    extensions: &["kt", "kts"],
    filenames: &[],
    grammar: Some(tree_sitter_kotlin_ng::LANGUAGE),
    callee_query: Some(CALLEE_QUERY),
    sibling_query: None,
    stdlib: StdlibRule::None,
    manifests: &[],
    definition_kinds: DEFAULT_DEF_KINDS,
    has_lifetimes: false,
    strip_family: Some(StripFamily::JavaKotlinCSharp),
    extract_receiver: None,
    definitions: DEFAULT_DEFS,
};
