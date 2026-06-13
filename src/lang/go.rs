//! Go language spec. Diverges on: stdlib rule and method-receiver extraction.

use streaming_iterator::StreamingIterator;

use crate::lang::spec::{LangSpec, StdlibRule, StripFamily, DEFAULT_DEFS, DEFAULT_DEF_KINDS};

const CALLEE_QUERY: &str = concat!(
    "(call_expression function: (identifier) @callee)\n",
    "(call_expression function: (selector_expression field: (field_identifier) @callee))\n",
);

const SIBLING_QUERY: &str =
    "(selector_expression operand: (identifier) @recv field: (field_identifier) @ref)\n";

/// Root (first `/`-segment) of each Go stdlib package. A Go import is stdlib
/// when its first path segment is one of these — covering both single-segment
/// (`fmt`) and multi-segment (`net/http`, `encoding/json`) forms. Matching the
/// root (not the whole path) avoids misclassifying a local package like
/// `mypackage` while still suppressing the noisy multi-segment stdlib paths.
const GO_STDLIB_ROOTS: &[&str] = &[
    "archive",
    "bufio",
    "bytes",
    "cmp",
    "compress",
    "container",
    "context",
    "crypto",
    "database",
    "debug",
    "embed",
    "encoding",
    "errors",
    "flag",
    "fmt",
    "go",
    "hash",
    "html",
    "image",
    "index",
    "io",
    "log",
    "maps",
    "math",
    "mime",
    "net",
    "os",
    "path",
    "plugin",
    "reflect",
    "regexp",
    "runtime",
    "slices",
    "sort",
    "strconv",
    "strings",
    "sync",
    "syscall",
    "testing",
    "text",
    "time",
    "unicode",
    "unsafe",
];
pub(crate) const SPEC: LangSpec = LangSpec {
    display: "Go",
    extensions: &["go"],
    filenames: &[],
    grammar: Some(tree_sitter_go::LANGUAGE),
    callee_query: Some(CALLEE_QUERY),
    sibling_query: Some(SIBLING_QUERY),
    stdlib: StdlibRule::GoRoots(GO_STDLIB_ROOTS),
    manifests: &["go.mod"],
    definition_kinds: DEFAULT_DEF_KINDS,
    has_lifetimes: false,
    strip_family: Some(StripFamily::Go),
    extract_receiver: Some(extract_go_receiver_name),
    definitions: DEFAULT_DEFS,
};

/// For Go methods, extract the receiver parameter name from the first method
/// in the file. Go receiver is the first parameter in `func (r *Type) Name()`.
pub(crate) fn extract_go_receiver_name(
    content: &str,
    ts_lang: &tree_sitter::Language,
) -> Option<String> {
    // `'static` so its pointer address is a stable cache key.
    const GO_RECV_QUERY: &str = "(method_declaration receiver: (parameter_list (parameter_declaration name: (identifier) @recv)))";

    let mut parser = tree_sitter::Parser::new();
    parser.set_language(ts_lang).ok()?;
    let tree = parser.parse(content, None)?;

    let bytes = content.as_bytes();

    // `with_query` returns `Option<Option<String>>`; flatten to `Option<String>`.
    crate::search::siblings::with_query(ts_lang, GO_RECV_QUERY, |query| {
        let recv_idx = query.capture_index_for_name("recv")?;
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), bytes);

        if let Some(m) = matches.next() {
            for cap in m.captures {
                if cap.index == recv_idx {
                    return cap.node.utf8_text(bytes).ok().map(String::from);
                }
            }
        }

        None
    })
    .flatten()
}
