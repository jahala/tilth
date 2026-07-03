//! Whole-file content tag: `[path#TAG]`.
//!
//! Ported from oh-my-pi `packages/hashline/src/format.ts:103-130`. The tag is a
//! 4-hex fingerprint of the whole file's normalized text: any read of
//! byte-identical content mints the same tag, and a follow-up edit validates
//! whenever the live file still hashes to it. Bit-compatibility with oh-my-pi's
//! output is not required — tilth mints and verifies its own tags.

#![allow(dead_code)]

use std::fmt::Write as _;

use twox_hash::XxHash32;

/// Number of hex characters in a content-derived file-hash tag.
pub const TAG_HEX_LEN: usize = 4;

/// Normalize text before hashing: strip trailing `[ \t\r]` from every line so
/// CRLF endings and display-trimmed lines do not invalidate a tag. Mirrors
/// oh-my-pi's `/[ \t\r]+(?=\n|$)/g` replace in a single pass.
fn normalize_for_hash(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // Buffer of pending trailing whitespace; flushed only when a non-trailing
    // char follows (i.e. it was interior whitespace, not line-trailing).
    let mut pending_ws = String::new();
    for ch in text.chars() {
        match ch {
            ' ' | '\t' | '\r' => pending_ws.push(ch),
            '\n' => {
                pending_ws.clear();
                out.push('\n');
            }
            _ => {
                out.push_str(&pending_ws);
                pending_ws.clear();
                out.push(ch);
            }
        }
    }
    // Trailing whitespace at end-of-text is line-trailing (matched by `$`).
    out
}

/// Compute the 16-bit content tag for `text`: normalize → xxHash32(seed 0) →
/// low 16 bits.
pub fn compute_file_hash(text: &str) -> u16 {
    let normalized = normalize_for_hash(text);
    let full = XxHash32::oneshot(0, normalized.as_bytes());
    (full & 0xffff) as u16
}

/// Format a 16-bit tag as 4-hex uppercase.
pub fn format_tag(tag: u16) -> String {
    format!("{tag:04X}")
}

/// Parse a 4-hex tag back to `u16`. Case-insensitive; rejects non-hex or
/// wrong-length input.
pub fn parse_tag(raw: &str) -> Option<u16> {
    if raw.len() != TAG_HEX_LEN {
        return None;
    }
    u16::from_str_radix(raw, 16).ok()
}

/// Format a hashline section header for a file path and tag: `[path#TAG]`.
pub fn format_header(path: &str, tag: u16) -> String {
    format!("[{path}#{}]", format_tag(tag))
}

/// Render a whole file as `N:content` numbered lines, splitting on `'\n'`
/// exactly the way [`crate::edit::apply::apply_line_ops`] does — the trailing
/// phantom empty row of a newline-terminated file is rendered as its own
/// numbered line. This keeps the displayed line numbers (and the recorded
/// seen-lines set) identical to the ones `apply` operates on, so a tag-matched
/// edit anchors the same rows the read displayed. Returns a trailing newline.
pub fn render_numbered_whole(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    for (i, line) in text.split('\n').enumerate() {
        let _ = writeln!(out, "{}:{line}", i + 1);
    }
    out
}

/// Render a selected slice as `N:content` numbered lines starting at `start`,
/// using `.lines()` so a slice that ends at a newline does not emit a spurious
/// trailing phantom row (the phantom is only meaningful for whole-file reads).
/// Returns a trailing newline.
pub fn render_numbered_slice(text: &str, start: u32) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    for (i, line) in text.lines().enumerate() {
        let _ = writeln!(out, "{}:{line}", start as usize + i);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_is_stable_round_trip() {
        let text = "fn main() {\n    println!(\"hi\");\n}\n";
        let a = compute_file_hash(text);
        let b = compute_file_hash(text);
        assert_eq!(a, b, "same content must mint the same tag");
        // Mint → format → parse → verify round-trips exactly.
        let hex = format_tag(a);
        assert_eq!(hex.len(), TAG_HEX_LEN);
        assert_eq!(parse_tag(&hex), Some(a));
    }

    #[test]
    fn trailing_whitespace_does_not_change_tag() {
        let clean = "let x = 1;\nlet y = 2;\n";
        let trailing = "let x = 1;   \nlet y = 2;\t\n";
        assert_eq!(
            compute_file_hash(clean),
            compute_file_hash(trailing),
            "trailing whitespace per line must normalize away"
        );
    }

    #[test]
    fn crlf_matches_lf() {
        assert_eq!(
            compute_file_hash("a\nb\nc\n"),
            compute_file_hash("a\r\nb\r\nc\r\n"),
            "CRLF is stripped by trailing-ws normalization"
        );
    }

    #[test]
    fn interior_carriage_return_is_significant() {
        // A `\r` NOT immediately before `\n` is interior content, not a line
        // ending, so it must change the tag — unlike the CRLF case above.
        assert_ne!(
            compute_file_hash("a\rb\n"),
            compute_file_hash("ab\n"),
            "interior carriage return is content, not a stripped line ending"
        );
    }

    #[test]
    fn interior_whitespace_is_significant() {
        // Whitespace that is NOT line-trailing must still affect the tag.
        assert_ne!(
            compute_file_hash("a b\n"),
            compute_file_hash("ab\n"),
            "interior whitespace changes content"
        );
    }

    #[test]
    fn content_change_changes_tag() {
        assert_ne!(
            compute_file_hash("hello\n"),
            compute_file_hash("world\n"),
            "different content must mint a different tag"
        );
    }

    #[test]
    fn header_format_is_bracketed_uppercase() {
        let tag = compute_file_hash("x\n");
        let header = format_header("src/foo.rs", tag);
        assert_eq!(header, format!("[src/foo.rs#{:04X}]", tag));
        assert!(header.starts_with("[src/foo.rs#"));
        assert!(header.ends_with(']'));
    }

    #[test]
    fn parse_tag_rejects_malformed() {
        assert_eq!(parse_tag("1A2"), None, "too short");
        assert_eq!(parse_tag("1A2B5"), None, "too long");
        assert_eq!(parse_tag("1A2G"), None, "non-hex");
        assert_eq!(parse_tag("1a2b"), Some(0x1a2b), "lowercase accepted");
    }
}
