//! Reading source files for parsing and search.
//!
//! A UTF-8 byte-order mark (U+FEFF, bytes `EF BB BF`) is an encoding marker,
//! not text. Left in place it sits in front of line 1, so `str::trim_start`
//! keeps it (U+FEFF is not white space), a line-1 `#include` or `import`
//! stops being one, `serde_json` refuses the file, and search output prints
//! a stray invisible character. Every read that parses or searches a file
//! goes through here and drops the mark. Views that print hash anchors keep
//! the file's bytes as they are, because `tilth_write` verifies its anchors
//! against the file on disk.

use std::io::{self, Read};
use std::path::Path;

const BOM: &str = "\u{feff}";

/// Files larger than this are not parsed or searched: symbol, content and
/// regex search, callers, callees, deps and the parse cache all skip them,
/// and a read outlines them with at most 100 entries. The one limit for all
/// of them; only Go's same-package callee scan, which parses up to 20
/// sibling files per call, keeps a tighter bound. Search, callers, grok and
/// deps name a skipped file that holds the query, so a skip is never silent
/// there.
pub const MAX_FILE_SIZE: u64 = 500_000;

/// `MAX_FILE_SIZE` as people read it, for notes that name the limit.
#[must_use]
pub fn max_file_size_label() -> String {
    format!("{} KB", MAX_FILE_SIZE / 1000)
}

/// How much of a file the size check holds at once, beyond the overlap.
const CHUNK: usize = 1 << 20;

/// Bytes of context a finder may read on each side of a hit: one UTF-8
/// character, enough for a word boundary, `^` or `$`.
const EDGE: usize = 4;

/// Does the file at `path` hold one of `words` as a whole word, with no
/// letter, digit or `_` joined to it on either side? This is how search, callers,
/// grok and deps tell whether a file they skip for size holds what they
/// look for. It streams the file, so its size costs no memory.
#[must_use]
pub fn holds_word(path: &Path, words: &[&str]) -> bool {
    let longest = words.iter().map(|w| w.len()).max().unwrap_or(0);
    holds(path, longest, |window, from| {
        words
            .iter()
            .filter(|w| !w.is_empty())
            .filter_map(|w| next_word(window, from, w.as_bytes()))
            .min_by_key(|&(start, _)| start)
    })
}

/// Does the file at `path` hold a hit of `find`? A file that cannot be
/// read holds nothing, and so does a binary one: a NUL byte in its first
/// 512 bytes, the rule every read uses.
///
/// The file is read in chunks of 1 MiB, each starting with the last
/// `overlap` bytes (plus a few) of the one before, so the check holds about
/// 1 MiB whatever the file's size or line length. `find(window, from)`
/// returns the first hit at or after `from` as a byte range. A hit near a
/// chunk's cut is not trusted, since its context lies past the cut; one no
/// longer than `overlap` is found again whole in the neighbouring chunk.
/// A longer hit that runs past a cut ends the search in that chunk, so
/// `overlap` must cover the longest hit that matters.
pub fn holds(
    path: &Path,
    overlap: usize,
    find: impl FnMut(&[u8], usize) -> Option<(usize, usize)>,
) -> bool {
    std::fs::File::open(path).is_ok_and(|file| stream_holds(file, overlap, find))
}

/// [`holds`] over any reader.
pub fn stream_holds(
    mut reader: impl Read,
    overlap: usize,
    mut find: impl FnMut(&[u8], usize) -> Option<(usize, usize)>,
) -> bool {
    let carry = overlap + 2 * EDGE;
    let mut buf: Vec<u8> = Vec::with_capacity(carry + CHUNK);
    // The file offset of `buf[0]`.
    let mut offset = 0usize;
    loop {
        let kept = buf.len();
        buf.resize(kept + CHUNK, 0);
        let mut filled = kept;
        while filled < buf.len() {
            match reader.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        let at_end = filled < buf.len();
        buf.truncate(filled);
        let at_start = offset == 0;
        if at_start && crate::lang::detection::is_binary(&buf) {
            return false;
        }
        let mut from = 0;
        while let Some((start, end)) = find(&buf, from) {
            if !at_end && end + EDGE > buf.len() {
                break;
            }
            if at_start || start >= EDGE {
                return true;
            }
            from = start + 1;
        }
        if at_end {
            return false;
        }
        let cut = buf.len().saturating_sub(carry);
        buf.drain(..cut);
        offset += cut;
    }
}

/// The first place at or after `from` where `word` stands in `window` with
/// a word boundary on each side. The window's own ends count as
/// boundaries; [`stream_holds`] does not trust a hit there.
fn next_word(window: &[u8], from: usize, word: &[u8]) -> Option<(usize, usize)> {
    let finder = memchr::memmem::Finder::new(word);
    let mut at = from;
    while at <= window.len() {
        let start = at + finder.find(&window[at..])?;
        let end = start + word.len();
        let before = (1..=start.min(EDGE))
            .find_map(|k| std::str::from_utf8(&window[start - k..start]).ok())
            .and_then(|s| s.chars().next_back());
        let after = (1..=(window.len() - end).min(EDGE))
            .find_map(|k| std::str::from_utf8(&window[end..end + k]).ok())
            .and_then(|s| s.chars().next());
        let word_char = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        let edge_inside = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || !c.is_ascii();
        let bounded_start = !edge_inside(word[0]) || !word_char(before);
        let bounded_end = !edge_inside(word[word.len() - 1]) || !word_char(after);
        if bounded_start && bounded_end {
            return Some((start, end));
        }
        at = start + 1;
    }
    None
}

/// `text` without a leading byte-order mark./// `text` without a leading byte-order mark.
#[must_use]
pub fn strip_bom(text: &str) -> &str {
    text.strip_prefix(BOM).unwrap_or(text)
}

/// `bytes` without a leading UTF-8 byte-order mark.
#[must_use]
pub fn strip_bom_bytes(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(BOM.as_bytes()).unwrap_or(bytes)
}

/// Read a UTF-8 file for parsing or search, without its byte-order mark.
pub fn read_source(path: &Path) -> io::Result<String> {
    let mut text = std::fs::read_to_string(path)?;
    if text.starts_with(BOM) {
        text.drain(..BOM.len());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word_in(reader: impl Read, word: &str) -> bool {
        stream_holds(reader, word.len(), |w, from| next_word(w, from, word.as_bytes()))
    }

    /// The check stops at the first hit. These readers never end, so a
    /// check that read the whole input would never return.
    #[test]
    fn the_check_stops_at_the_first_hit_on_endless_input() {
        let line = io::Cursor::new(b"first line\nneedle here\n".to_vec()).chain(io::repeat(b'\n'));
        assert!(word_in(line, "needle"));
        let one_line = io::Cursor::new(b"var a=needle;".to_vec()).chain(io::repeat(b'a'));
        assert!(word_in(one_line, "needle"));
    }

    /// A NUL byte in the first 512 bytes makes the input binary, which
    /// holds nothing; one further on is a stray byte in a text file.
    #[test]
    fn only_a_nul_near_the_start_makes_the_input_binary() {
        assert!(!word_in(&b"header\0needle\n"[..], "needle"));
        let mut late = vec![b'x'; 600];
        late.extend_from_slice(b"\0 needle\n");
        assert!(word_in(&late[..], "needle"));
    }

    /// Every cut between chunks, whatever bytes it falls between, gives
    /// the same answer as a search of the whole input.
    #[test]
    fn a_cut_between_chunks_changes_no_answer() {
        for (text, word, holds) in [
            ("needle", "needle", true),
            ("xneedle", "needle", false),
            ("needlex", "needle", false),
            ("éneedle", "needle", false),
            ("→needle←", "needle", true),
            ("needle_v2 needle", "needle", true),
            ("needle_v2 xneedle", "needle", false),
        ] {
            // The first cut falls at `CHUNK`, and the next chunk starts a
            // little over the word's length before it.
            for at in (0..4).chain(CHUNK - 30..=CHUNK + 4) {
                let mut input = vec![b' '; at];
                input.extend_from_slice(text.as_bytes());
                input.extend_from_slice(b" end");
                assert_eq!(word_in(&input[..], word), holds, "{text:?} at {at}");
            }
        }
    }

    #[test]
    fn only_a_leading_mark_is_dropped() {
        assert_eq!(strip_bom("\u{feff}use a;"), "use a;");
        assert_eq!(strip_bom("use a; // \u{feff}"), "use a; // \u{feff}");
        assert_eq!(strip_bom_bytes(b"\xef\xbb\xbfx"), b"x");
        assert_eq!(strip_bom_bytes(b"x"), b"x");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "\u{feff}use a;\n").unwrap();
        assert_eq!(read_source(&path).unwrap(), "use a;\n");
    }
}
