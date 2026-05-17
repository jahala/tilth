//! Path suffix grammar: `path#suffix` parsing for `tilth_read` / `tilth_search`.
//!
//! Suffix forms (after a `#`):
//!   * empty       → whole-file read
//!   * `n-m`       → 1-indexed inclusive line range
//!   * `n`         → from line n to end of file
//!   * `# heading` → markdown heading anchor
//!   * `name`      → code symbol resolved via outline

use std::path::PathBuf;

/// Suffix forms accepted on a path string after a `#`.
#[derive(Debug, Clone)]
pub enum PathSuffix {
    /// Whole-file read (no suffix).
    None,
    /// `#n-m` — line range start..=end (1-indexed inclusive).
    LineRange(usize, usize),
    /// `#n` — from line n to end of file.
    FromLine(usize),
    /// `#<heading text>` — markdown heading (the leading `#` is in the suffix).
    Heading(String),
    /// `#<symbol name>` — code symbol resolved via outline.
    Symbol(String),
}

/// Split `"path#suffix"` into `(path, suffix)`. When the suffix is purely
/// numeric it is parsed as a line address; otherwise heading vs symbol
/// disambiguation is left to the caller (depends on file type).
pub fn parse_path_with_suffix(spec: &str) -> (PathBuf, PathSuffix) {
    let Some(hash_idx) = spec.find('#') else {
        return (PathBuf::from(spec), PathSuffix::None);
    };
    let path = PathBuf::from(&spec[..hash_idx]);
    let suffix_raw = &spec[hash_idx + 1..];
    if suffix_raw.is_empty() {
        return (path, PathSuffix::None);
    }

    // numeric line forms (no leading `#` here — caller already stripped one)
    if let Some((a, b)) = suffix_raw.split_once('-') {
        if let (Ok(start), Ok(end)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
            if start >= 1 && end >= start {
                return (path, PathSuffix::LineRange(start, end));
            }
        }
    }
    if let Ok(n) = suffix_raw.trim().parse::<usize>() {
        if n >= 1 {
            return (path, PathSuffix::FromLine(n));
        }
    }

    // If the suffix begins with `#` it's a markdown heading anchor (the
    // user wrote `path### Foo` to denote level-2 heading "Foo"). Strip the
    // leading character we already consumed: the heading function expects
    // its own `#` prefix style. We pass the full `# ...` form along.
    if suffix_raw.starts_with('#') {
        return (path, PathSuffix::Heading(suffix_raw.to_string()));
    }

    // Heuristic split between heading and symbol:
    //  * a non-empty suffix with internal whitespace → heading text
    //  * otherwise → symbol name
    if suffix_raw.contains(' ') {
        // Reinject a `# ` so it looks like an ATX heading to the resolver.
        return (path, PathSuffix::Heading(format!("# {suffix_raw}")));
    }
    (path, PathSuffix::Symbol(suffix_raw.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_no_suffix() {
        let (p, s) = parse_path_with_suffix("src/foo.rs");
        assert_eq!(p, PathBuf::from("src/foo.rs"));
        assert!(matches!(s, PathSuffix::None));
    }

    #[test]
    fn parse_line_range() {
        let (p, s) = parse_path_with_suffix("a.rs#10-20");
        assert_eq!(p, PathBuf::from("a.rs"));
        assert!(matches!(s, PathSuffix::LineRange(10, 20)));
    }

    #[test]
    fn parse_from_line() {
        let (_, s) = parse_path_with_suffix("a.rs#42");
        assert!(matches!(s, PathSuffix::FromLine(42)));
    }

    #[test]
    fn parse_heading() {
        let (_, s) = parse_path_with_suffix("README.md### Foo Bar");
        if let PathSuffix::Heading(h) = s {
            assert!(h.contains("Foo Bar"));
        } else {
            panic!("expected heading");
        }
    }

    #[test]
    fn parse_symbol() {
        let (_, s) = parse_path_with_suffix("src/foo.rs#do_thing");
        assert!(matches!(s, PathSuffix::Symbol(name) if name == "do_thing"));
    }

    #[test]
    fn parse_no_suffix_empty_after_hash() {
        // `path#` with nothing after — treat as no suffix, not a malformed range.
        let (p, s) = parse_path_with_suffix("a.rs#");
        assert_eq!(p, PathBuf::from("a.rs"));
        assert!(
            matches!(s, PathSuffix::None),
            "empty suffix → None, got {s:?}"
        );
    }

    #[test]
    fn parse_invalid_range_falls_through_to_symbol() {
        // `#10-5` (end < start) is not a valid range; falls through to symbol.
        let (_, s) = parse_path_with_suffix("a.rs#10-5");
        match s {
            PathSuffix::Symbol(_) | PathSuffix::Heading(_) => {}
            other => panic!("invalid range must not produce LineRange, got {other:?}"),
        }
    }

    #[test]
    fn parse_from_line_zero_rejected() {
        // Line 0 is not a valid 1-indexed line; falls through to symbol form.
        let (_, s) = parse_path_with_suffix("a.rs#0");
        assert!(
            !matches!(s, PathSuffix::FromLine(_)),
            "line 0 must not be FromLine, got {s:?}"
        );
    }
}
