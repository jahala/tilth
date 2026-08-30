//! Python language spec. Diverges on: stdlib first-segment rule (`.` separator).

use crate::lang::spec::{LangSpec, StdlibRule, StripFamily, DEFAULT_DEFS, DEFAULT_DEF_KINDS};

const CALLEE_QUERY: &str = concat!(
    "(call function: (identifier) @callee)\n",
    "(call function: (attribute attribute: (identifier) @callee))\n",
);

const SIBLING_QUERY: &str = "(attribute object: (identifier) @obj attribute: (identifier) @ref)\n";

/// Common stdlib modules — not exhaustive, but covers the noisy ones.
const STDLIB_MODULES: &[&str] = &[
    "os",
    "sys",
    "re",
    "json",
    "math",
    "time",
    "datetime",
    "pathlib",
    "typing",
    "collections",
    "functools",
    "itertools",
    "abc",
    "io",
    "logging",
    "unittest",
    "dataclasses",
    "enum",
    "copy",
    "hashlib",
    "subprocess",
    "threading",
    "asyncio",
];

pub(crate) const SPEC: LangSpec = LangSpec {
    display: "Python",
    extensions: &["py", "pyi"],
    filenames: &[],
    grammar: Some(tree_sitter_python::LANGUAGE),
    callee_query: Some(CALLEE_QUERY),
    sibling_query: Some(SIBLING_QUERY),
    stdlib: StdlibRule::PythonSegment(STDLIB_MODULES),
    manifests: &["pyproject.toml", "setup.py"],
    definition_kinds: DEFAULT_DEF_KINDS,
    has_lifetimes: false,
    strip_family: Some(StripFamily::Python),
    extract_receiver: None,
    definitions: DEFAULT_DEFS,
};
