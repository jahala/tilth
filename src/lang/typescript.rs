//! TypeScript language spec. Shares callee/sibling queries with JS/TSX.

use crate::lang::javascript::{CALLEE_QUERY, SIBLING_QUERY};
use crate::lang::spec::{LangSpec, StdlibRule, StripFamily, DEFAULT_DEFS, DEFAULT_DEF_KINDS};

pub(crate) const SPEC: LangSpec = LangSpec {
    display: "TypeScript",
    extensions: &["ts"],
    filenames: &[],
    grammar: Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
    callee_query: Some(CALLEE_QUERY),
    sibling_query: Some(SIBLING_QUERY),
    stdlib: StdlibRule::None,
    manifests: &[],
    definition_kinds: DEFAULT_DEF_KINDS,
    has_lifetimes: false,
    strip_family: Some(StripFamily::JsTs),
    extract_receiver: None,
    definitions: DEFAULT_DEFS,
};
