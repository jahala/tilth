//! Per-language data table. One `LangSpec` per `Lang` variant, looked up by the
//! single `spec(lang)` match — the only surviving full-`Lang` dispatch in the
//! crate. Every former `match lang` site reads a field on `spec(lang)` instead.
//!
//! Shared, language-agnostic behavior (the default definition kinds, default
//! definition-name extraction and weight) lives once as `DEFAULT_*` items; only
//! the languages that diverge carry their own values.

use std::path::Path;

use tree_sitter_language::LanguageFn;

use crate::lang::treesitter::{
    definition_weight as default_definition_weight,
    extract_definition_name as default_extract_definition_name, DEFINITION_KINDS,
};
use crate::types::Lang;

/// Signature of [`LangSpec::test_site`]: the captured node, the file's bytes,
/// the file's path, and the search scope that path is judged relative to.
pub(crate) type TestSiteFn = fn(&tree_sitter::Node, &[u8], &Path, &Path) -> bool;

/// All per-language data and behavior in one record. Read via `spec(lang)`.
pub(crate) struct LangSpec {
    /// Human-readable language name (`lang_display_name`).
    pub display: &'static str,
    /// File extensions that map to this language (`detect_file_type`).
    pub extensions: &'static [&'static str],
    /// Bare filenames that map to this language (`file_type_from_name`).
    pub filenames: &'static [&'static str],
    /// Tree-sitter grammar, or `None` for languages without a shipped grammar
    /// (Dockerfile / Make). `outline_language` does `.map(Into::into)`.
    pub grammar: Option<LanguageFn>,
    /// Tree-sitter query for callee extraction (`callee_query_str`).
    pub callee_query: Option<&'static str>,
    /// Tree-sitter query for self/this sibling references (`sibling_query_str`).
    pub sibling_query: Option<&'static str>,
    /// How to recognise a stdlib import for this language (`is_stdlib`).
    pub stdlib: StdlibRule,
    /// Build-manifest filenames contributed by this language (`package_root`).
    pub manifests: &'static [&'static str],
    /// Definition node kinds for AST definition detection (`DEFINITION_KINDS`).
    pub definition_kinds: &'static [&'static str],
    /// Whether `'` denotes a lifetime tick rather than a char delimiter
    /// (`Lang::has_lifetimes`).
    pub has_lifetimes: bool,
    /// Coarse comment-syntax family for cognitive-load stripping (`StripLang`).
    pub strip_family: Option<StripFamily>,
    /// Go-only: extract the method receiver name from file content.
    pub extract_receiver: Option<fn(&str, &tree_sitter::Language) -> Option<String>>,
    /// Rust-only: reject a `@callee` capture that `callee_query` matched too
    /// loosely. Consumers of the capture apply it when present.
    pub callee_filter: Option<fn(&tree_sitter::Node) -> bool>,
    /// Rust-only: whether a call site is test code by a signal only the source
    /// carries — a `#[test]` function, a `#[cfg(test)]` module, or a file under
    /// a `tests` directory. Languages whose tests `is_test_file` recognises
    /// from the filename alone leave this `None`.
    pub test_site: Option<TestSiteFn>,
    /// Definition-name extraction + weight (Elixir overrides the defaults).
    pub definitions: DefinitionOps,
}

/// How an import source is recognised as standard-library (and thus noise that
/// `tilth_deps` suppresses). Replaces the per-language `is_stdlib` match. Each
/// variant encodes one language's historical rule byte-for-byte.
pub(crate) enum StdlibRule {
    /// Language has no stdlib suppression.
    None,
    /// Source matches if it starts with any of these prefixes (Rust:
    /// `std::` / `core::` / `alloc::`).
    Prefixes(&'static [&'static str]),
    /// Source matches if its first `'.'`-delimited segment is in `set` (Python).
    PythonSegment(&'static [&'static str]),
    /// Source matches if its first `'/'`-delimited segment is in `set` (Go:
    /// the import's root path segment is a stdlib package root).
    GoRoots(&'static [&'static str]),
}

impl StdlibRule {
    /// Returns `true` if `source` names a standard-library module under this rule.
    pub(crate) fn matches(&self, source: &str) -> bool {
        match self {
            StdlibRule::None => false,
            StdlibRule::Prefixes(prefixes) => prefixes.iter().any(|p| source.starts_with(p)),
            StdlibRule::PythonSegment(set) => {
                let first = source.split('.').next().unwrap_or("");
                set.contains(&first)
            }
            StdlibRule::GoRoots(set) => set.contains(&source.split('/').next().unwrap_or(source)),
        }
    }
}

/// Coarse 6-way grouping of comment / log syntax for `strip::strip_noise`.
/// Deliberately *not* a 1:1 sibling of `Lang` (many languages share a family).
#[derive(Debug, Clone, Copy)]
pub(crate) enum StripFamily {
    Rust,
    Python,
    Go,
    JsTs,
    JavaKotlinCSharp,
    CppC,
    Bash,
}

/// Definition-name extraction + semantic weight for a language. The default
/// (`DEFAULT_DEFS`) walks standard field names; Elixir overrides both because
/// its definitions are `call` nodes.
pub(crate) struct DefinitionOps {
    /// Extract the defined symbol name from a definition node.
    pub extract_name: fn(tree_sitter::Node, &[&str]) -> Option<String>,
    /// Semantic ranking weight for a definition node.
    pub weight: fn(tree_sitter::Node, &[&str]) -> u16,
}

/// Shared default definition kinds — used by every language except Elixir, Go,
/// C and C++.
pub(crate) const DEFAULT_DEF_KINDS: &[&str] = DEFINITION_KINDS;

/// Default weight adapter: weight is keyed on the node kind, so the node's
/// `lines` argument is unused. Kept in the `(node, lines)` shape so every
/// `DefinitionOps.weight` has one uniform signature.
fn default_defs_weight(node: tree_sitter::Node, _lines: &[&str]) -> u16 {
    default_definition_weight(node.kind())
}

/// Shared default `DefinitionOps` — referenced by every non-Elixir spec.
pub(crate) const DEFAULT_DEFS: DefinitionOps = DefinitionOps {
    extract_name: default_extract_definition_name,
    weight: default_defs_weight,
};

/// The single surviving full-`Lang` dispatch. Every other former `match lang`
/// reads a field on the returned spec.
pub(crate) fn spec(lang: Lang) -> &'static LangSpec {
    use crate::lang;
    match lang {
        Lang::Rust => &lang::rust::SPEC,
        Lang::TypeScript => &lang::typescript::SPEC,
        Lang::Tsx => &lang::tsx::SPEC,
        Lang::JavaScript => &lang::javascript::SPEC,
        Lang::Python => &lang::python::SPEC,
        Lang::Go => &lang::go::SPEC,
        Lang::Java => &lang::java::SPEC,
        Lang::Scala => &lang::scala::SPEC,
        Lang::C => &lang::c::SPEC,
        Lang::Cpp => &lang::cpp::SPEC,
        Lang::Ruby => &lang::ruby::SPEC,
        Lang::Php => &lang::php::SPEC,
        Lang::Swift => &lang::swift::SPEC,
        Lang::Kotlin => &lang::kotlin::SPEC,
        Lang::CSharp => &lang::csharp::SPEC,
        Lang::Elixir => &lang::elixir::SPEC,
        Lang::Dockerfile => &lang::dockerfile::SPEC,
        Lang::Make => &lang::make::SPEC,
        Lang::Bash => &lang::bash::SPEC,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::mod_all_langs_for_test;

    // ── StdlibRule::matches — locks each variant's historical `is_stdlib` rule ──

    #[test]
    fn stdlib_none_never_matches() {
        let rule = StdlibRule::None;
        assert!(!rule.matches("std"));
        assert!(!rule.matches(""));
        assert!(!rule.matches("anything.at.all"));
    }

    #[test]
    fn stdlib_prefixes_matches_only_listed_prefixes() {
        let rule = StdlibRule::Prefixes(&["std::", "core::", "alloc::"]);
        assert!(rule.matches("std::collections::HashMap"));
        assert!(rule.matches("core::mem"));
        assert!(rule.matches("alloc::vec::Vec"));
        // Bare segment without `::` separator is not a prefix match.
        assert!(!rule.matches("std"));
        // A crate that merely starts with the letters but not the prefix.
        assert!(!rule.matches("standard::thing"));
        assert!(!rule.matches("serde::Deserialize"));
    }

    #[test]
    fn stdlib_python_segment_matches_first_dotted_segment() {
        let rule = StdlibRule::PythonSegment(&["os", "sys", "json"]);
        assert!(rule.matches("os"));
        assert!(rule.matches("os.path"));
        assert!(rule.matches("json.decoder"));
        // First segment must match exactly — a longer name sharing a prefix must not.
        assert!(!rule.matches("ossify"));
        assert!(!rule.matches("requests"));
        assert!(!rule.matches("mypackage.os"));
    }

    // Go's rule (post-PR-71): a GoRoots allowlist keyed on the first `/`-segment.
    // A multi-segment import (`net/http`) is stdlib when its root (`net`) is in the
    // set; a bare local package (`mypackage`) is NOT stdlib. These pin that rule.
    #[test]
    fn stdlib_go_root_segment_is_stdlib() {
        let rule = StdlibRule::GoRoots(&["fmt", "net", "encoding"]);
        assert!(rule.matches("fmt"), "bare stdlib root");
        assert!(rule.matches("net/http"), "multi-segment stdlib via root");
        assert!(
            rule.matches("encoding/json"),
            "multi-segment stdlib via root"
        );
    }

    #[test]
    fn stdlib_go_non_root_is_not_stdlib() {
        let rule = StdlibRule::GoRoots(&["fmt", "net"]);
        // Local/third-party roots are not in the set.
        assert!(
            !rule.matches("mypackage"),
            "bare local package is not stdlib"
        );
        assert!(!rule.matches("github.com/gin-gonic/gin"));
        assert!(!rule.matches("golang.org/x/sync"));
        // Empty string has no stdlib root.
        assert!(!rule.matches(""));
        // A root that merely shares a prefix with a stdlib root must not match.
        assert!(!rule.matches("fmtlib"));
    }

    // ── spec() table invariants — every Lang resolves and its data is coherent ──

    #[test]
    fn spec_resolves_for_every_lang() {
        // Exercises the single surviving full-`Lang` dispatch over the whole set.
        for &lang in mod_all_langs_for_test() {
            let s = spec(lang);
            assert!(!s.display.is_empty(), "{lang:?} has empty display name");
        }
    }

    #[test]
    fn only_dockerfile_and_make_lack_a_grammar() {
        for &lang in mod_all_langs_for_test() {
            let has_grammar = spec(lang).grammar.is_some();
            let expected = !matches!(lang, Lang::Dockerfile | Lang::Make);
            assert_eq!(
                has_grammar, expected,
                "{lang:?} grammar presence mismatch (Dockerfile/Make have no shipped grammar)"
            );
        }
    }

    #[test]
    fn only_rust_has_lifetimes() {
        for &lang in mod_all_langs_for_test() {
            assert_eq!(
                spec(lang).has_lifetimes,
                matches!(lang, Lang::Rust),
                "{lang:?} lifetime flag mismatch — only Rust uses `'` for lifetime ticks"
            );
        }
    }

    #[test]
    fn only_go_extracts_a_receiver() {
        for &lang in mod_all_langs_for_test() {
            assert_eq!(
                spec(lang).extract_receiver.is_some(),
                matches!(lang, Lang::Go),
                "{lang:?} receiver-extractor presence mismatch — only Go has method receivers"
            );
        }
    }

    #[test]
    fn only_rust_filters_callee_captures() {
        for &lang in mod_all_langs_for_test() {
            assert_eq!(
                spec(lang).callee_filter.is_some(),
                matches!(lang, Lang::Rust),
                "{lang:?} callee-filter presence mismatch — only Rust's callee \
                 query has a pattern loose enough to need one"
            );
        }
    }

    #[test]
    fn only_rust_detects_test_sites() {
        for &lang in mod_all_langs_for_test() {
            assert_eq!(
                spec(lang).test_site.is_some(),
                matches!(lang, Lang::Rust),
                "{lang:?} test-site detector presence mismatch — only Rust marks \
                 test code by attribute and `tests/` directory rather than by \
                 the filename conventions `is_test_file` knows"
            );
        }
    }

    #[test]
    fn extensions_are_unique_across_langs() {
        // detect_file_type scans ALL_LANGS and returns the first lang whose
        // `spec.extensions` contains the ext. Overlapping extensions would make
        // detection order-dependent and silently misroute files.
        let mut seen: Vec<(&str, Lang)> = Vec::new();
        for &lang in mod_all_langs_for_test() {
            for &ext in spec(lang).extensions {
                if let Some((_, other)) = seen.iter().find(|(e, _)| *e == ext) {
                    panic!("extension {ext:?} claimed by both {other:?} and {lang:?}");
                }
                seen.push((ext, lang));
            }
        }
    }

    #[test]
    fn only_elixir_go_and_c_family_override_definition_kinds() {
        // The refactor's shared-default-plus-override design: every spec points at
        // DEFAULT_DEF_KINDS except Elixir, whose definitions are `call` nodes, Go,
        // whose `type`/`const`/`var` names live on specs inside a declaration,
        // and C/C++, whose types are defined by `*_specifier` nodes with a body.
        for &lang in mod_all_langs_for_test() {
            let uses_default = spec(lang).definition_kinds == DEFAULT_DEF_KINDS;
            assert_eq!(
                uses_default,
                !matches!(lang, Lang::Elixir | Lang::Go | Lang::C | Lang::Cpp),
                "{lang:?} definition_kinds override mismatch — only Elixir, Go, C and C++ diverge"
            );
        }
    }

    // ── Pinned per-language data the dispatch sites read ──

    #[test]
    fn pinned_spec_fields() {
        assert_eq!(spec(Lang::Rust).display, "Rust");
        assert_eq!(spec(Lang::Rust).extensions, &["rs"]);
        assert_eq!(spec(Lang::Rust).manifests, &["Cargo.toml"]);

        assert_eq!(spec(Lang::Go).extensions, &["go"]);
        assert_eq!(spec(Lang::Go).manifests, &["go.mod"]);
        assert!(spec(Lang::Go).extract_receiver.is_some());

        assert_eq!(spec(Lang::Python).extensions, &["py", "pyi"]);
        assert_eq!(
            spec(Lang::Python).manifests,
            &["pyproject.toml", "setup.py"]
        );

        assert_eq!(spec(Lang::Elixir).manifests, &["mix.exs"]);
        assert!(spec(Lang::Elixir).grammar.is_some());
    }
}
