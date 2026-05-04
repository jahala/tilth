//! Per-language tree-sitter queries for matching call expressions, with a
//! global compiled-`Query` cache. Used by the caller-direction walk
//! (`callers::find_callers_batch`) and the callee-direction extractor
//! (`callees::extract_callee_names`) — they share both the query strings
//! and the compiled-query cache.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::types::Lang;

/// Return the tree-sitter query string for extracting callee names in the given language.
/// Each language has patterns targeting `@callee` captures on call-like expressions.
pub(super) fn callee_query_str(lang: Lang) -> Option<&'static str> {
    match lang {
        Lang::Rust => Some(concat!(
            "(call_expression function: (identifier) @callee)\n",
            "(call_expression function: (field_expression field: (field_identifier) @callee))\n",
            "(call_expression function: (scoped_identifier name: (identifier) @callee))\n",
            "(macro_invocation macro: (identifier) @callee)\n",
        )),
        Lang::Go => Some(concat!(
            "(call_expression function: (identifier) @callee)\n",
            "(call_expression function: (selector_expression field: (field_identifier) @callee))\n",
        )),
        Lang::Python => Some(concat!(
            "(call function: (identifier) @callee)\n",
            "(call function: (attribute attribute: (identifier) @callee))\n",
        )),
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => Some(concat!(
            "(call_expression function: (identifier) @callee)\n",
            "(call_expression function: (member_expression property: (property_identifier) @callee))\n",
        )),
        Lang::Java => Some(
            "(method_invocation name: (identifier) @callee)\n",
        ),
        Lang::Scala => Some(concat!(
            "(call_expression function: (identifier) @callee)\n",
            "(call_expression function: (field_expression field: (identifier) @callee))\n",
            "(infix_expression operator: (identifier) @callee)\n",
        )),
        Lang::C | Lang::Cpp => Some(concat!(
            "(call_expression function: (identifier) @callee)\n",
            "(call_expression function: (field_expression field: (field_identifier) @callee))\n",
        )),
        Lang::Ruby => Some(
            "(call method: (identifier) @callee)\n",
        ),
        Lang::Php => Some(concat!(
            "(function_call_expression function: (name) @callee)\n",
            "(function_call_expression function: (qualified_name) @callee)\n",
            "(function_call_expression function: (relative_name) @callee)\n",
            "(member_call_expression name: (name) @callee)\n",
            "(nullsafe_member_call_expression name: (name) @callee)\n",
            "(scoped_call_expression name: (name) @callee)\n",
        )),
        Lang::CSharp => Some(concat!(
            "(invocation_expression function: (identifier) @callee)\n",
            "(invocation_expression function: (member_access_expression name: (identifier) @callee))\n",
        )),
        Lang::Swift => Some(concat!(
            "(call_expression (simple_identifier) @callee)\n",
            "(call_expression (navigation_expression suffix: (navigation_suffix suffix: (simple_identifier) @callee)))\n",
        )),
        Lang::Kotlin => Some(concat!(
            "(call_expression (identifier) @callee)\n",
            "(call_expression (navigation_expression (identifier) @callee .))\n",
        )),
        Lang::Elixir => Some(concat!(
            "(call target: (identifier) @callee)\n",
            "(call target: (dot right: (identifier) @callee))\n",
        )),
        _ => None,
    }
}

/// Global cache of compiled tree-sitter queries for callee extraction.
///
/// Keyed by `(symbol_count, field_count)` — a pair that uniquely identifies
/// each grammar in practice. We avoid keying by `Language::name()` because
/// older grammars (ABI < 15) do not register a name and would return `None`,
/// silently disabling the cache and callee extraction entirely.
///
/// `Query` is `Send + Sync` in tree-sitter 0.25, so a global `Mutex`-guarded
/// map is safe and avoids recompiling the same query on every call.
static QUERY_CACHE: LazyLock<Mutex<HashMap<(usize, usize), tree_sitter::Query>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Stable cache key for a tree-sitter language. Uses `(symbol_count,
/// field_count)` which is unique for every grammar shipped with tilth.
fn lang_cache_key(ts_lang: &tree_sitter::Language) -> (usize, usize) {
    (ts_lang.node_kind_count(), ts_lang.field_count())
}

/// Look up or compile the callee query for `ts_lang`, then invoke `f` with a
/// reference to the cached `Query`.  Returns `None` if compilation fails.
pub(super) fn with_callee_query<R>(
    ts_lang: &tree_sitter::Language,
    query_str: &str,
    f: impl FnOnce(&tree_sitter::Query) -> R,
) -> Option<R> {
    let key = lang_cache_key(ts_lang);
    let mut cache = QUERY_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let std::collections::hash_map::Entry::Vacant(e) = cache.entry(key) {
        let query = tree_sitter::Query::new(ts_lang, query_str).ok()?;
        e.insert(query);
    }
    // Safety: we just inserted if absent, so the key is always present here.
    Some(f(cache.get(&key).expect("just inserted")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_cache_keys_unique() {
        // Verify that (node_kind_count, field_count) is unique across all shipped grammars.
        // A collision would cause one language to serve another's cached query.
        let grammars: Vec<(&str, tree_sitter::Language)> = vec![
            ("rust", tree_sitter_rust::LANGUAGE.into()),
            (
                "typescript",
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            ),
            ("tsx", tree_sitter_typescript::LANGUAGE_TSX.into()),
            ("javascript", tree_sitter_javascript::LANGUAGE.into()),
            ("python", tree_sitter_python::LANGUAGE.into()),
            ("go", tree_sitter_go::LANGUAGE.into()),
            ("java", tree_sitter_java::LANGUAGE.into()),
            ("c", tree_sitter_c::LANGUAGE.into()),
            ("cpp", tree_sitter_cpp::LANGUAGE.into()),
            ("ruby", tree_sitter_ruby::LANGUAGE.into()),
            ("php", tree_sitter_php::LANGUAGE_PHP.into()),
            ("scala", tree_sitter_scala::LANGUAGE.into()),
            ("csharp", tree_sitter_c_sharp::LANGUAGE.into()),
            ("swift", tree_sitter_swift::LANGUAGE.into()),
            ("kotlin", tree_sitter_kotlin_ng::LANGUAGE.into()),
            ("elixir", tree_sitter_elixir::LANGUAGE.into()),
        ];
        let mut seen = std::collections::HashMap::new();
        for (name, lang) in &grammars {
            let key = lang_cache_key(lang);
            if let Some(prev) = seen.insert(key, name) {
                panic!("cache key collision: {prev} and {name} both produce {key:?}");
            }
        }
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
}
