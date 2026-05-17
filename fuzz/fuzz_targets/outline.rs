#![no_main]
//! Fuzz `outline` rendering across every supported language.
//!
//! Iterates each input through all 18 `Lang` variants. The render path
//! exercises tree-sitter parsing, AST traversal, signature extraction,
//! and the formatter — i.e. tilth's primary input surface ("any file on
//! disk in any language").
//!
//! Findings expected: tree-sitter wrapper edge cases (multi-byte UTF-8
//! boundaries, oversized nodes, deeply nested children), formatter
//! panics on unusual symbol names, off-by-ones in the `max_lines` budget.

use libfuzzer_sys::fuzz_target;
use tilth::__fuzz::{outline, Lang};

const LANGS: &[Lang] = &[
    Lang::Rust,
    Lang::TypeScript,
    Lang::Tsx,
    Lang::JavaScript,
    Lang::Python,
    Lang::Go,
    Lang::Java,
    Lang::Scala,
    Lang::C,
    Lang::Cpp,
    Lang::Ruby,
    Lang::Php,
    Lang::Swift,
    Lang::Kotlin,
    Lang::CSharp,
    Lang::Elixir,
    Lang::Dockerfile,
    Lang::Make,
];

fuzz_target!(|data: &[u8]| {
    let Ok(content) = std::str::from_utf8(data) else {
        return;
    };
    for &lang in LANGS {
        let _ = outline(content, lang, 1000);
    }
});
