//! C# language spec.

use crate::lang::spec::{LangSpec, StdlibRule, StripFamily, DEFAULT_DEFS, DEFAULT_DEF_KINDS};

const CALLEE_QUERY: &str = concat!(
    "(invocation_expression function: (identifier) @callee)\n",
    "(invocation_expression function: (member_access_expression name: (identifier) @callee))\n",
);

const SIBLING_QUERY: &str = concat!(
    "(member_access_expression expression: (this_expression) name: (identifier) @ref)\n",
    "(invocation_expression function: (member_access_expression expression: (this_expression) name: (identifier) @ref))\n",
);

pub(crate) const SPEC: LangSpec = LangSpec {
    display: "C#",
    extensions: &["cs"],
    filenames: &[],
    grammar: Some(tree_sitter_c_sharp::LANGUAGE),
    callee_query: Some(CALLEE_QUERY),
    sibling_query: Some(SIBLING_QUERY),
    stdlib: StdlibRule::None,
    manifests: &[],
    definition_kinds: DEFAULT_DEF_KINDS,
    has_lifetimes: false,
    strip_family: Some(StripFamily::JavaKotlinCSharp),
    extract_receiver: None,
    definitions: DEFAULT_DEFS,
};
