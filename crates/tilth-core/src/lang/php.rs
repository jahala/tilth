//! PHP language spec.

use crate::lang::spec::{LangSpec, StdlibRule, DEFAULT_DEFS, DEFAULT_DEF_KINDS};

const CALLEE_QUERY: &str = concat!(
    "(function_call_expression function: (name) @callee)\n",
    "(function_call_expression function: (qualified_name) @callee)\n",
    "(function_call_expression function: (relative_name) @callee)\n",
    "(member_call_expression name: (name) @callee)\n",
    "(nullsafe_member_call_expression name: (name) @callee)\n",
    "(scoped_call_expression name: (name) @callee)\n",
);

pub(crate) const SPEC: LangSpec = LangSpec {
    display: "PHP",
    extensions: &["php", "phtml"],
    filenames: &[],
    grammar: Some(tree_sitter_php::LANGUAGE_PHP),
    callee_query: Some(CALLEE_QUERY),
    sibling_query: None,
    stdlib: StdlibRule::None,
    manifests: &["composer.json"],
    definition_kinds: DEFAULT_DEF_KINDS,
    has_lifetimes: false,
    strip_family: None,
    extract_receiver: None,
    definitions: DEFAULT_DEFS,
};
