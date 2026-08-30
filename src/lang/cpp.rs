//! C++ language spec. Shares its callee query with C.

use crate::lang::c::CALLEE_QUERY;
use crate::lang::spec::{LangSpec, StdlibRule, StripFamily, DEFAULT_DEFS, DEFAULT_DEF_KINDS};

pub(crate) const SPEC: LangSpec = LangSpec {
    display: "C++",
    extensions: &["cpp", "hpp", "cc", "cxx"],
    filenames: &[],
    grammar: Some(tree_sitter_cpp::LANGUAGE),
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
