//! Per-language tree-sitter queries for matching call expressions, shared by
//! the caller-direction walk (`callers::find_callers_batch`) and the
//! callee-direction extractor (`callees::extract_callee_names`). Both compile
//! them through `lang::treesitter::compiled_query`.

use crate::types::Lang;

/// Return the tree-sitter query string for extracting callee names in the given language.
/// Each language has patterns targeting `@callee` captures on call-like expressions.
pub(super) fn callee_query_str(lang: Lang) -> Option<&'static str> {
    crate::lang::spec::spec(lang).callee_query
}

#[cfg(test)]
mod tests {
    use streaming_iterator::StreamingIterator;

    use super::*;

    /// tree-sitter parses a macro invocation's arguments as a `token_tree` of
    /// raw tokens, so a call inside `write!(..)` has no `call_expression` node
    /// for the other patterns to match. Compiling the query here is not
    /// redundant: `compiled_query` swallows a compile error with `.ok()?`,
    /// which would silently disable callers and callees for Rust rather than
    /// fail.
    #[test]
    fn rust_callee_query_finds_calls_in_macro_arguments() {
        let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        let query_str = callee_query_str(Lang::Rust).unwrap();
        let query =
            tree_sitter::Query::new(&lang, query_str).expect("rust callee query should compile");
        let callee_idx = query.capture_index_for_name("callee").unwrap();

        let src =
            "fn f(out: &mut String, p: &P) { let _ = write!(out, \"{}\", Capped(&p.query)); }";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(src, None).unwrap();

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), src.as_bytes());
        let mut names: Vec<&str> = Vec::new();
        while let Some(m) = matches.next() {
            for cap in m.captures() {
                if cap.index == callee_idx {
                    names.push(cap.node.utf8_text(src.as_bytes()).unwrap());
                }
            }
        }

        assert!(
            names.contains(&"Capped"),
            "call inside a macro argument not captured: {names:?}"
        );
    }

    #[test]
    fn kotlin_callee_query_compiles() {
        let lang: tree_sitter::Language = tree_sitter_kotlin_ng::LANGUAGE.into();
        let query_str = callee_query_str(Lang::Kotlin).unwrap();
        tree_sitter::Query::new(&lang, query_str).expect("kotlin callee query should compile");
    }

    #[test]
    fn elixir_callee_query_compiles() {
        let lang: tree_sitter::Language = tree_sitter_elixir::LANGUAGE.into();
        let query_str = callee_query_str(Lang::Elixir).unwrap();
        tree_sitter::Query::new(&lang, query_str).expect("elixir callee query should compile");
    }

    #[test]
    fn bash_callee_query_compiles() {
        let lang: tree_sitter::Language = tree_sitter_bash::LANGUAGE.into();
        let query_str = callee_query_str(Lang::Bash).unwrap();
        tree_sitter::Query::new(&lang, query_str).expect("bash callee query should compile");
    }
}
