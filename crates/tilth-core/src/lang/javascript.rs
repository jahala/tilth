//! JavaScript language spec. Shares callee/sibling queries with TS/TSX.

use crate::lang::spec::{LangSpec, StdlibRule, StripFamily, DEFAULT_DEFS, DEFAULT_DEF_KINDS};

/// Callee query shared by JavaScript, TypeScript, and TSX.
pub(crate) const CALLEE_QUERY: &str = concat!(
    "(call_expression function: (identifier) @callee)\n",
    "(call_expression function: (member_expression property: (property_identifier) @callee))\n",
);

/// Sibling (`this.x`) query shared by JavaScript, TypeScript, and TSX.
pub(crate) const SIBLING_QUERY: &str =
    "(member_expression object: (this) property: (property_identifier) @ref)\n";

pub(crate) const SPEC: LangSpec = LangSpec {
    display: "JavaScript",
    extensions: &["js", "jsx"],
    filenames: &[],
    grammar: Some(tree_sitter_javascript::LANGUAGE),
    callee_query: Some(CALLEE_QUERY),
    sibling_query: Some(SIBLING_QUERY),
    stdlib: StdlibRule::None,
    manifests: &["package.json"],
    definition_kinds: DEFAULT_DEF_KINDS,
    has_lifetimes: false,
    strip_family: Some(StripFamily::JsTs),
    extract_receiver: None,
    definitions: DEFAULT_DEFS,
};
