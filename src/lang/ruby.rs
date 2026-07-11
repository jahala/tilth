//! Ruby language spec.

use crate::lang::spec::{LangSpec, StdlibRule, DEFAULT_DEFS, DEFAULT_DEF_KINDS};

const CALLEE_QUERY: &str = "(call method: (identifier) @callee)\n";

pub(crate) const SPEC: LangSpec = LangSpec {
    display: "Ruby",
    extensions: &["rb"],
    filenames: &["Vagrantfile", "Rakefile"],
    grammar: Some(tree_sitter_ruby::LANGUAGE),
    callee_query: Some(CALLEE_QUERY),
    sibling_query: None,
    stdlib: StdlibRule::None,
    manifests: &[],
    definition_kinds: DEFAULT_DEF_KINDS,
    has_lifetimes: false,
    strip_family: None,
    extract_receiver: None,
    definitions: DEFAULT_DEFS,
};
