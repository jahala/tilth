//! Cognitive load stripping — removes noise (logging, redundant comments,
//! consecutive blank lines) from expanded function bodies to reduce token count.
//!
//! All detection is line-by-line text matching; no tree-sitter needed.

use std::collections::HashSet;
use std::path::Path;

use crate::lang::detect_file_type;
use crate::lang::spec::{spec, StripFamily};
use crate::types::FileType;

/// Detect the stripping comment-syntax family for a file.
///
/// Routes through `detect_file_type` → `spec(lang).strip_family` for every
/// extension the detector recognises. The `.mjs` / `.cjs` extensions are
/// JavaScript that `detect_file_type` does not classify (it treats them as
/// non-code), so they are mapped to the JS/TS family here to preserve the
/// historical strip behavior for those files.
fn detect_lang(path: &Path) -> Option<StripFamily> {
    if let FileType::Code(lang) = detect_file_type(path) {
        if let Some(family) = spec(lang).strip_family {
            return Some(family);
        }
    }
    match path.extension()?.to_str()? {
        "mjs" | "cjs" => Some(StripFamily::JsTs),
        _ => None,
    }
}

/// Returns the set of 1-based line numbers to skip when rendering an expanded
/// function body. Only lines within `def_range` are considered.
///
/// Returns an empty set if:
/// - `def_range` is `None`
/// - The file extension maps to an unsupported language
pub(crate) fn strip_noise(
    content: &str,
    path: &Path,
    def_range: Option<(u32, u32)>,
) -> HashSet<u32> {
    let mut skip = HashSet::new();

    let Some((range_start, range_end)) = def_range else {
        return skip;
    };

    let Some(lang) = detect_lang(path) else {
        return skip;
    };

    let lines: Vec<&str> = content.lines().collect();
    let mut consecutive_blanks: u32 = 0;

    for line_num in range_start..=range_end {
        let idx = (line_num - 1) as usize;
        let line = match lines.get(idx) {
            Some(l) => *l,
            None => break,
        };

        let trimmed = line.trim();

        // --- Rule (a): Consecutive blank line collapse ---
        if trimmed.is_empty() {
            consecutive_blanks += 1;
            if consecutive_blanks >= 2 {
                skip.insert(line_num);
            }
            continue;
        }
        consecutive_blanks = 0;

        // --- Rule (b): Logging/debug stripping ---
        if is_debug_log(trimmed, lang) {
            skip.insert(line_num);
            continue;
        }

        // --- Rule (c): Inline comment stripping ---
        if is_strippable_comment(trimmed, lang) {
            skip.insert(line_num);
        }
    }

    skip
}

/// Returns `true` if the line is a debug/trace logging statement that should
/// be stripped. Only matches lines that are *only* a log call (not part of a
/// larger expression).
fn is_debug_log(trimmed: &str, lang: StripFamily) -> bool {
    match lang {
        StripFamily::Rust => {
            trimmed.starts_with("log::debug!")
                || trimmed.starts_with("log::trace!")
                || trimmed.starts_with("tracing::debug!")
                || trimmed.starts_with("tracing::trace!")
                || trimmed.starts_with("debug!(")
                || trimmed.starts_with("trace!(")
                || trimmed.starts_with("dbg!(")
        }
        StripFamily::Python => {
            trimmed.starts_with("logger.debug(")
                || trimmed.starts_with("logging.debug(")
                || trimmed.starts_with("print(")
                || trimmed.starts_with("pprint(")
                || trimmed.starts_with("pprint.pprint(")
        }
        StripFamily::Go => {
            trimmed.starts_with("log.Printf(")
                || trimmed.starts_with("log.Println(")
                || trimmed.starts_with("log.Print(")
                || trimmed.starts_with("fmt.Printf(")
                || trimmed.starts_with("fmt.Println(")
                || trimmed.starts_with("fmt.Print(")
        }
        StripFamily::JsTs => {
            trimmed.starts_with("console.log(")
                || trimmed.starts_with("console.debug(")
                || trimmed.starts_with("console.trace(")
        }
        StripFamily::JavaKotlinCSharp => {
            // Java: System.out.println, logger.debug, log.debug
            trimmed.starts_with("System.out.print")
                || trimmed.starts_with("logger.debug(")
                || trimmed.starts_with("log.debug(")
                || trimmed.starts_with("Log.d(")
                || trimmed.starts_with("println(") // Kotlin, Scala
        }
        StripFamily::CppC => {
            trimmed.starts_with("printf(")
                || trimmed.starts_with("std::cout")
                || trimmed.starts_with("cout ")
                || trimmed.starts_with("cout<<")
        }
    }
}

/// Annotations that protect a comment from being stripped.
const KEEP_MARKERS: &[&str] = &["TODO", "FIXME", "NOTE", "HACK", "SAFETY", "WARN"];

/// Returns `true` if the line is a plain comment that should be stripped.
/// Preserves: doc comments, comments containing keep-markers.
fn is_strippable_comment(trimmed: &str, lang: StripFamily) -> bool {
    let is_comment = match lang {
        StripFamily::Rust => {
            // Doc comments: `///`, `//!`, `/** */`, `#[doc`
            if trimmed.starts_with("///")
                || trimmed.starts_with("//!")
                || trimmed.starts_with("/**")
                || trimmed.starts_with("#[doc")
            {
                return false;
            }
            trimmed.starts_with("//")
        }
        StripFamily::Python => {
            // Doc strings: `"""`, `'''`
            if trimmed.starts_with("\"\"\"") || trimmed.starts_with("'''") {
                return false;
            }
            trimmed.starts_with('#')
        }
        StripFamily::Go => trimmed.starts_with("//"),
        StripFamily::JsTs => {
            // Doc comments: `/**`, `* ` (JSDoc continuation)
            if trimmed.starts_with("/**") || trimmed.starts_with("* ") || trimmed == "*/" {
                return false;
            }
            trimmed.starts_with("//")
        }
        StripFamily::JavaKotlinCSharp => {
            // Doc comments: `/**`, `///` (C#)
            if trimmed.starts_with("/**") || trimmed.starts_with("///") {
                return false;
            }
            trimmed.starts_with("//")
        }
        StripFamily::CppC => {
            // Doxygen: `/**`, `///`, `//!`
            if trimmed.starts_with("/**")
                || trimmed.starts_with("///")
                || trimmed.starts_with("//!")
            {
                return false;
            }
            trimmed.starts_with("//")
        }
    };

    if !is_comment {
        return false;
    }

    // Keep comments containing important markers
    let upper = trimmed.to_ascii_uppercase();
    !KEEP_MARKERS.iter().any(|m| upper.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn path(ext: &str) -> PathBuf {
        PathBuf::from(format!("test.{ext}"))
    }

    #[test]
    fn consecutive_blanks_collapsed() {
        let content = "fn foo() {\n    let x = 1;\n\n\n\n    let y = 2;\n}\n";
        let skip = strip_noise(content, &path("rs"), Some((1, 6)));
        // Lines 3,4,5 are blank; keep first (3), skip 4 and 5
        assert!(!skip.contains(&3));
        assert!(skip.contains(&4));
        assert!(skip.contains(&5));
    }

    #[test]
    fn rust_debug_log_stripped() {
        let content = "fn foo() {\n    debug!(\"hi\");\n    dbg!(x);\n    error!(\"bad\");\n}\n";
        let skip = strip_noise(content, &path("rs"), Some((1, 5)));
        assert!(skip.contains(&2)); // debug!
        assert!(skip.contains(&3)); // dbg!
        assert!(!skip.contains(&4)); // error! kept
    }

    #[test]
    fn js_console_log_stripped() {
        let content = "function foo() {\n  console.log('hi');\n  console.error('bad');\n}\n";
        let skip = strip_noise(content, &path("ts"), Some((1, 4)));
        assert!(skip.contains(&2)); // console.log
        assert!(!skip.contains(&3)); // console.error kept
    }

    #[test]
    fn python_print_stripped() {
        let content = "def foo():\n    print(x)\n    logger.error('bad')\n";
        let skip = strip_noise(content, &path("py"), Some((1, 3)));
        assert!(skip.contains(&2)); // print
        assert!(!skip.contains(&3)); // logger.error kept
    }

    #[test]
    fn go_fmt_println_stripped() {
        let content = "func foo() {\n\tfmt.Println(\"debug\")\n\tlog.Fatalf(\"fatal\")\n}\n";
        let skip = strip_noise(content, &path("go"), Some((1, 4)));
        assert!(skip.contains(&2)); // fmt.Println
        assert!(!skip.contains(&3)); // log.Fatalf kept
    }

    #[test]
    fn comment_stripped_unless_marker() {
        let content =
            "fn foo() {\n    // just a comment\n    // TODO: fix this\n    /// doc comment\n}\n";
        let skip = strip_noise(content, &path("rs"), Some((1, 5)));
        assert!(skip.contains(&2)); // plain comment stripped
        assert!(!skip.contains(&3)); // TODO kept
        assert!(!skip.contains(&4)); // doc comment kept
    }

    #[test]
    fn no_range_returns_empty() {
        let content = "fn foo() {}\n";
        let skip = strip_noise(content, &path("rs"), None);
        assert!(skip.is_empty());
    }

    #[test]
    fn unsupported_lang_returns_empty() {
        let content = "fn foo() {}\n";
        let skip = strip_noise(content, &path("txt"), Some((1, 1)));
        assert!(skip.is_empty());
    }

    #[test]
    fn ruby_not_supported() {
        let content = "def foo\n  puts 'hi'\nend\n";
        let skip = strip_noise(content, &path("rb"), Some((1, 3)));
        assert!(skip.is_empty());
    }

    #[test]
    fn jsdoc_continuation_preserved() {
        let content = "function f() {\n  /**\n   * JSDoc line\n   */\n  // plain comment\n}\n";
        let skip = strip_noise(content, &path("js"), Some((1, 6)));
        assert!(!skip.contains(&2)); // /**
        assert!(!skip.contains(&3)); // * JSDoc continuation
        assert!(!skip.contains(&4)); // */
        assert!(skip.contains(&5)); // plain comment
    }

    #[test]
    fn mjs_cjs_strip_as_js() {
        // `.mjs` / `.cjs` are JavaScript that `detect_file_type` classifies as
        // `Other`; the fallback shim in `detect_lang` maps them to the JsTs
        // family so they still strip `console.log` like `.js`/`.ts`.
        let content = "function f() {\n  console.log('hi');\n  console.error('bad');\n}\n";
        for ext in ["mjs", "cjs"] {
            let skip = strip_noise(content, &path(ext), Some((1, 4)));
            assert!(skip.contains(&2), "{ext}: console.log should strip");
            assert!(!skip.contains(&3), "{ext}: console.error should be kept");
        }
    }

    #[test]
    fn mjs_cjs_detect_lang_routes_to_jsts() {
        // Pin the fallback directly: these extensions resolve to the JsTs family
        // even though `detect_file_type` does not classify them as code.
        assert!(matches!(detect_lang(&path("mjs")), Some(StripFamily::JsTs)));
        assert!(matches!(detect_lang(&path("cjs")), Some(StripFamily::JsTs)));
    }
}
