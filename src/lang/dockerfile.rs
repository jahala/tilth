//! Dockerfile spec. No tree-sitter grammar shipped — outline returns `None`.

use crate::lang::spec::{LangSpec, StdlibRule, DEFAULT_DEFS, DEFAULT_DEF_KINDS};

pub(crate) const SPEC: LangSpec = LangSpec {
    display: "Docker",
    extensions: &[],
    filenames: &["Dockerfile", "Containerfile"],
    grammar: None,
    callee_query: None,
    sibling_query: None,
    stdlib: StdlibRule::None,
    manifests: &[],
    definition_kinds: DEFAULT_DEF_KINDS,
    has_lifetimes: false,
    strip_family: None,
    extract_receiver: None,
    definitions: DEFAULT_DEFS,
};
