//! Resolve a [`BlockAnchor`] to a concrete line span, wiring to tilth's
//! tree-sitter outline. The `#symbol` variant reuses the same resolution the
//! `#symbol` read selector uses (`get_outline_entries` + first name match); a
//! line anchor resolves to the outline block that begins on that line, else the
//! innermost outline block containing it.

#![allow(dead_code)]

use std::path::Path;

use super::parser::BlockAnchor;
use crate::lang::outline::{find_entry_by_name, get_outline_entries};
use crate::types::{FileType, OutlineEntry};

/// A resolved 1-based inclusive line span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockSpan {
    pub start: u32,
    pub end: u32,
}

/// Resolve `anchor` against `text` (parsed as the language inferred from
/// `path`). Returns `None` for an unknown language, an out-of-range or blank
/// line, or a symbol/line that resolves to no block.
pub fn resolve_block(path: &Path, text: &str, anchor: &BlockAnchor) -> Option<BlockSpan> {
    resolve_block_in(&outline_for(path, text)?, anchor)
}

/// Outline entries for `text` under the language inferred from `path`, or
/// `None` for a non-code file. Parsing the whole file is the expensive step, so
/// callers resolving several anchors should compute this once and reuse it via
/// [`resolve_block_in`].
pub fn outline_for(path: &Path, text: &str) -> Option<Vec<OutlineEntry>> {
    let FileType::Code(lang) = crate::lang::detect_file_type(path) else {
        return None;
    };
    Some(get_outline_entries(text, lang))
}

/// Resolve `anchor` against pre-computed outline `entries`.
pub fn resolve_block_in(entries: &[OutlineEntry], anchor: &BlockAnchor) -> Option<BlockSpan> {
    match anchor {
        BlockAnchor::Symbol(name) => {
            find_entry_by_name(entries, name).map(|(s, e)| BlockSpan { start: s, end: e })
        }
        BlockAnchor::Line(line) => {
            resolve_line(entries, *line).map(|(s, e)| BlockSpan { start: s, end: e })
        }
    }
}

/// Resolve a line anchor: prefer a block that *begins* on `line` (oh-my-pi's
/// "node begins here" semantics); otherwise the innermost block containing it.
fn resolve_line(entries: &[OutlineEntry], line: u32) -> Option<(u32, u32)> {
    if let Some(hit) = begins_on(entries, line) {
        return Some(hit);
    }
    innermost_containing(entries, line)
}

fn begins_on(entries: &[OutlineEntry], line: u32) -> Option<(u32, u32)> {
    for e in entries {
        // Prefer the deepest child that also begins on the line.
        if let Some(hit) = begins_on(&e.children, line) {
            return Some(hit);
        }
        if e.start_line == line {
            return Some((e.start_line, e.end_line));
        }
    }
    None
}

fn innermost_containing(entries: &[OutlineEntry], line: u32) -> Option<(u32, u32)> {
    for e in entries {
        if line >= e.start_line && line <= e.end_line {
            // A child span is strictly inside, so prefer it if it also contains.
            if let Some(hit) = innermost_containing(&e.children, line) {
                return Some(hit);
            }
            return Some((e.start_line, e.end_line));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn rs_path() -> PathBuf {
        // A `.rs` extension so detect_file_type picks the Rust grammar; the
        // path need not exist — resolution runs against `text`.
        PathBuf::from("resolve_block_fixture.rs")
    }

    const SRC: &str = "\
fn alpha() {
    let a = 1;
    a + 1
}

fn beta() {
    let b = 2;
    b + 2
}
";

    #[test]
    fn symbol_anchor_matches_read_selector_span() {
        let path = rs_path();
        let span =
            resolve_block(&path, SRC, &BlockAnchor::Symbol("beta".into())).expect("beta resolves");
        // The `#symbol` read selector resolves via the same outline entry, so
        // resolve_block must produce the identical (start,end).
        let entries = get_outline_entries(SRC, crate::types::Lang::Rust);
        let expected = find_entry_by_name(&entries, "beta").expect("beta in outline");
        assert_eq!((span.start, span.end), expected);
        // And the span actually covers beta's opener line (line 6).
        assert_eq!(span.start, 6);
    }

    #[test]
    fn line_anchor_on_opener_resolves_enclosing_block() {
        let path = rs_path();
        // Line 1 is `fn alpha() {` — the block opener.
        let span =
            resolve_block(&path, SRC, &BlockAnchor::Line(1)).expect("line 1 resolves to a block");
        assert_eq!(span.start, 1);
        assert_eq!(span.end, 4, "alpha spans lines 1-4");
    }

    #[test]
    fn line_anchor_inside_body_resolves_enclosing_block() {
        let path = rs_path();
        // Line 7 is `let b = 2;` inside beta (lines 6-9).
        let span = resolve_block(&path, SRC, &BlockAnchor::Line(7)).expect("line 7 resolves");
        assert_eq!((span.start, span.end), (6, 9));
    }

    #[test]
    fn unknown_symbol_yields_none() {
        let path = rs_path();
        assert!(resolve_block(&path, SRC, &BlockAnchor::Symbol("nonexistent".into())).is_none());
    }

    #[test]
    fn non_code_file_yields_none() {
        let path = PathBuf::from("data.bin.unknownext");
        assert!(resolve_block(&path, SRC, &BlockAnchor::Line(1)).is_none());
    }
}
