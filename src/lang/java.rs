//! Java language spec.

use crate::lang::spec::{LangSpec, StdlibRule, StripFamily, DEFAULT_DEFS, DEFAULT_DEF_KINDS};

const CALLEE_QUERY: &str = "(method_invocation name: (identifier) @callee)\n";

const SIBLING_QUERY: &str = concat!(
    "(field_access object: (this) field: (identifier) @ref)\n",
    "(method_invocation object: (this) name: (identifier) @ref)\n",
);

pub(crate) const SPEC: LangSpec = LangSpec {
    display: "Java",
    extensions: &["java"],
    filenames: &[],
    grammar: Some(tree_sitter_java::LANGUAGE),
    callee_query: Some(CALLEE_QUERY),
    sibling_query: Some(SIBLING_QUERY),
    stdlib: StdlibRule::None,
    manifests: &["pom.xml", "build.gradle"],
    definition_kinds: DEFAULT_DEF_KINDS,
    has_lifetimes: false,
    strip_family: Some(StripFamily::JavaKotlinCSharp),
    extract_receiver: None,
    definitions: DEFAULT_DEFS,
};
