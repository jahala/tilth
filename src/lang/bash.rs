//! Bash language spec.

use crate::lang::spec::{LangSpec, StdlibRule, StripFamily, DEFAULT_DEFS, DEFAULT_DEF_KINDS};

const CALLEE_QUERY: &str = "(command name: (command_name) @callee)\n";

pub(crate) const SPEC: LangSpec = LangSpec {
    display: "Bash",
    extensions: &["sh", "bash", "bats"],
    filenames: &[".bashrc", ".bash_profile", ".bash_aliases", ".profile"],
    grammar: Some(tree_sitter_bash::LANGUAGE),
    callee_query: Some(CALLEE_QUERY),
    sibling_query: None,
    stdlib: StdlibRule::None,
    manifests: &[],
    definition_kinds: DEFAULT_DEF_KINDS,
    has_lifetimes: false,
    strip_family: Some(StripFamily::Bash),
    extract_receiver: None,
    definitions: DEFAULT_DEFS,
};
