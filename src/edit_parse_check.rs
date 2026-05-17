//! Post-edit tree-sitter parse check.
//!
//! After `tilth_edit` writes a file, parse the pre- and post-edit content with
//! the language's tree-sitter grammar and surface any *new* `ERROR` / `MISSING`
//! nodes the edit introduced. Pre-existing errors stay silent.
//!
//! Multiset diff by `(ErrorKind, detail)` where `ErrorKind` is the local
//! `Error`/`Missing` enum (not the tree-sitter node kind) and `detail` is the
//! trimmed/truncated node text. Deliberately non-positional — robust to edits
//! that shift line numbers (a positional key would false-flag pre-existing
//! errors below the edit site).

use std::collections::HashMap;
use std::path::Path;

use crate::lang::detect_file_type;
use crate::lang::outline::outline_language;
use crate::types::FileType;

/// Maximum number of new errors listed before truncating to a count summary.
const MAX_LISTED: usize = 10;

/// Maximum characters of error-node text shown in the `detail` field.
const DETAIL_MAX_CHARS: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    Error,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// 1-indexed line number, suitable for direct display.
    pub line: usize,
    /// 0-indexed column from `tree_sitter::Point::column`. Internal sort key
    /// only — never rendered, so the indexing offset doesn't appear in output.
    pub col: usize,
    pub kind: ErrorKind,
    /// For `ERROR`: trimmed/truncated node text. For `MISSING`: the node `kind()`
    /// (i.e. what was expected, e.g. `;`).
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct ParseReport {
    pub new_errors: Vec<ParseError>,
    /// Total errors in the post-edit parse, used for the truncation summary.
    pub total_post: usize,
}

/// Parse `before` and `after`, return a report of errors introduced by the
/// edit. Returns `None` when:
/// - the path's language has no tree-sitter grammar, or
/// - no new errors were introduced.
pub fn check(path: &Path, before: &str, after: &str) -> Option<ParseReport> {
    let FileType::Code(lang) = detect_file_type(path) else {
        return None;
    };
    let grammar = outline_language(lang)?;

    let pre = parse_errors(&grammar, before)?;
    let post = parse_errors(&grammar, after)?;

    let total_post = post.len();
    let new_errors = multiset_subtract(&pre, post);
    if new_errors.is_empty() {
        return None;
    }

    Some(ParseReport {
        new_errors,
        total_post,
    })
}

/// Format a report as a `── parse ──` block, caps at `MAX_LISTED` lines.
pub fn format_report(report: &ParseReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("\u{2500}\u{2500} parse \u{2500}\u{2500}");
    for e in report.new_errors.iter().take(MAX_LISTED) {
        let kind = match e.kind {
            ErrorKind::Error => "ERROR",
            ErrorKind::Missing => "MISSING",
        };
        if e.detail.is_empty() {
            let _ = write!(out, "\n:{} {kind}", e.line);
        } else {
            let _ = write!(out, "\n:{} {kind} {}", e.line, e.detail);
        }
    }
    let total_new = report.new_errors.len();
    if total_new > MAX_LISTED {
        let more = total_new - MAX_LISTED;
        let total_post = report.total_post;
        let _ = write!(
            out,
            "\n... and {more} more ({total_new} new, {total_post} total)"
        );
    }
    out
}

fn parse_errors(grammar: &tree_sitter::Language, source: &str) -> Option<Vec<ParseError>> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(grammar).ok()?;
    let tree = parser.parse(source, None)?;
    let mut errors = Vec::new();
    collect_errors(tree.root_node(), source.as_bytes(), &mut errors);
    errors.sort_by_key(|e| (e.line, e.col));
    Some(errors)
}

fn collect_errors(node: tree_sitter::Node, source: &[u8], out: &mut Vec<ParseError>) {
    if node.is_error() || node.is_missing() {
        let pos = node.start_position();
        let (kind, detail) = if node.is_missing() {
            (ErrorKind::Missing, node.kind().to_string())
        } else {
            let text = node.utf8_text(source).unwrap_or("").trim();
            (ErrorKind::Error, truncate_chars(text, DETAIL_MAX_CHARS))
        };
        out.push(ParseError {
            line: pos.row + 1,
            col: pos.column,
            kind,
            detail,
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_errors(child, source, out);
    }
}

/// Multiset subtraction. For each key `(ErrorKind, detail)` — i.e.
/// `(Error|Missing, trimmed node text or expected kind)` — count occurrences
/// in `pre`; walk `post` in order, skipping the first `pre_count` instances
/// of that key (they match pre-existing errors), and collecting the rest as
/// new. Positions are deliberately not part of the key.
fn multiset_subtract(pre: &[ParseError], post: Vec<ParseError>) -> Vec<ParseError> {
    let mut remaining: HashMap<(ErrorKind, String), usize> = HashMap::new();
    for e in pre {
        *remaining.entry((e.kind, e.detail.clone())).or_insert(0) += 1;
    }
    let mut new_errors = Vec::new();
    for e in post {
        let key = (e.kind, e.detail.clone());
        if let Some(count) = remaining.get_mut(&key) {
            if *count > 0 {
                *count -= 1;
                continue;
            }
        }
        new_errors.push(e);
    }
    new_errors
}

fn truncate_chars(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(max);
    for (i, c) in s.chars().enumerate() {
        if i >= max {
            return format!("{out}…");
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn rust(name: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/{name}.rs"))
    }

    #[test]
    fn clean_edit_returns_none() {
        let before = "fn a() { 1 }\nfn b() { 2 }\n";
        let after = "fn a() { 10 }\nfn b() { 2 }\n";
        assert!(check(&rust("clean"), before, after).is_none());
    }

    #[test]
    fn introduced_error_reported() {
        let before = "fn a() { 1 }\n";
        let after = "fn a() { 1 \n"; // missing closing brace
        let r = check(&rust("brace"), before, after).expect("should report");
        assert!(!r.new_errors.is_empty(), "expected new errors");
        assert!(r
            .new_errors
            .iter()
            .any(|e| matches!(e.kind, ErrorKind::Error | ErrorKind::Missing)),);
    }

    #[test]
    fn preexisting_error_silent() {
        // File starts broken (missing brace at end), edit unrelated line.
        let before = "fn a() { 1 }\nfn b() { 2 \n";
        let after = "fn a() { 99 }\nfn b() { 2 \n";
        assert!(
            check(&rust("preexisting"), before, after).is_none(),
            "pre-existing error should not be flagged"
        );
    }

    #[test]
    fn pre_existing_error_shifted_by_edit_is_silent() {
        // Pre-existing trailing error. The edit deletes a line above it,
        // shifting the error's line number down. Positional diff would
        // false-flag; multiset diff stays silent.
        let before = "fn a() { 1 }\nfn b() { 2 }\nfn c() { 3 \n";
        let after = "fn b() { 2 }\nfn c() { 3 \n"; // dropped fn a
        assert!(
            check(&rust("shifted"), before, after).is_none(),
            "shifted pre-existing error should not be flagged",
        );
    }

    #[test]
    fn missing_node_reported() {
        // Java grammar reports MISSING for omitted semicolons.
        let path = PathBuf::from("/tmp/T.java");
        let before = "class T { int x = 1; }\n";
        let after = "class T { int x = 1 }\n";
        let r = check(&path, before, after).expect("should report");
        assert!(
            r.new_errors
                .iter()
                .any(|e| matches!(e.kind, ErrorKind::Missing)),
            "expected MISSING node, got {:?}",
            r.new_errors
        );
    }

    #[test]
    fn format_caps_at_ten_with_summary() {
        // tree-sitter coalesces consecutive bad tokens into one ERROR node, so
        // crafting 15 separate errors from source is grammar-dependent and
        // fragile. Test format_report directly with a synthetic report — the
        // cap is the unit under test, not the parser.
        let new_errors: Vec<ParseError> = (1..=15)
            .map(|i| ParseError {
                line: i,
                col: 0,
                kind: ErrorKind::Error,
                detail: "}".into(),
            })
            .collect();
        let report = ParseReport {
            new_errors,
            total_post: 15,
        };
        let formatted = format_report(&report);
        let listed = formatted.lines().filter(|l| l.starts_with(':')).count();
        assert_eq!(listed, MAX_LISTED, "expected exactly 10 listed");
        assert!(
            formatted.contains("... and 5 more (15 new, 15 total)"),
            "summary missing: {formatted}",
        );
    }

    #[test]
    fn no_grammar_returns_none() {
        // .txt files have no grammar (FileType::Other).
        let path = PathBuf::from("/tmp/notes.txt");
        let before = "hello\n";
        let after = "hello there\n";
        assert!(check(&path, before, after).is_none());
    }

    #[test]
    fn dockerfile_returns_none() {
        let path = PathBuf::from("/tmp/Dockerfile");
        let before = "FROM scratch\n";
        let after = "FROM scratch\nCMD echo hi\n";
        assert!(check(&path, before, after).is_none());
    }

    #[test]
    fn error_detail_truncated() {
        // Construct an error with a long literal that becomes the error text.
        // Rust grammar treats unbalanced parens as ERROR — wrap in a function
        // and inject a long broken expression.
        let long = "x".repeat(200);
        let before = "fn a() { 1 }\n";
        let after = format!("fn a() {{ ({long} \n");
        let r = check(&rust("trunc"), before, &after).expect("should report");
        for e in &r.new_errors {
            assert!(
                e.detail.chars().count() <= DETAIL_MAX_CHARS + 1,
                "detail too long: {:?}",
                e.detail
            );
        }
    }

    #[test]
    fn errors_sorted_by_line_then_col() {
        // Synthetic input — sort is a property of parse_errors() and the
        // formatter output. Construct an unsorted list, run it through
        // multiset_subtract (which preserves order) and verify the post-parse
        // list is sorted in check(). Easier to assert directly on parse_errors
        // by re-parsing a known-broken file.
        let path = rust("sort");
        let before = "fn a() { 1 }\n";
        // Two unrelated syntax errors on different lines.
        let after = "fn a() { ) }\nfn b() { ( }\n";
        let r = check(&path, before, after).expect("should report new errors");
        assert!(
            r.new_errors.len() >= 2,
            "expected >=2 errors, got {:?}",
            r.new_errors
        );
        let mut last: (usize, usize) = (0, 0);
        for e in &r.new_errors {
            assert!(
                (e.line, e.col) >= last,
                "errors not sorted: {:?}",
                r.new_errors
            );
            last = (e.line, e.col);
        }
    }

    #[test]
    fn format_omits_block_header_only_on_no_errors() {
        // format_report is only called when new_errors is non-empty (check returns
        // None otherwise). This guards the contract — format starts with the header.
        let report = ParseReport {
            new_errors: vec![ParseError {
                line: 5,
                col: 0,
                kind: ErrorKind::Error,
                detail: "}".into(),
            }],
            total_post: 1,
        };
        let s = format_report(&report);
        assert!(s.starts_with("\u{2500}\u{2500} parse \u{2500}\u{2500}"));
        assert!(s.contains(":5 ERROR"));
        assert!(s.contains('}'));
    }
}
