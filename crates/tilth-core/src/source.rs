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
/// regex search, callers, callees, deps and the parse cache all skip them.
/// The one limit for all of them. Search names a skipped file that holds the
/// query, so a skip is never silent there.
pub const MAX_FILE_SIZE: u64 = 500_000;

/// `MAX_FILE_SIZE` as people read it, for notes that name the limit.
#[must_use]
pub fn max_file_size_label() -> String {
    format!("{} KB", MAX_FILE_SIZE / 1000)
}

/// `text` without a leading byte-order mark.
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

/// How many bytes [`stream_contains`] reads at a time.
const STREAM_CHUNK: usize = 64 * 1024;

/// Whether `reader` holds `needle` anywhere. It reads in bounded chunks, so
/// memory stays at one chunk whatever the input's size, and it stops at the
/// first match. The last `needle.len() - 1` bytes of each chunk carry over,
/// so a match split across two reads is still found. An empty needle is in
/// every input.
///
/// # Errors
///
/// Returns the reader's I/O error. An interrupted read is retried.
pub fn stream_contains(mut reader: impl Read, needle: &[u8]) -> io::Result<bool> {
    if needle.is_empty() {
        return Ok(true);
    }
    let finder = memchr::memmem::Finder::new(needle);
    let carry_max = needle.len() - 1;
    let mut buf = vec![0u8; carry_max + STREAM_CHUNK];
    let mut filled = 0;
    loop {
        let read = match reader.read(&mut buf[filled..]) {
            Ok(0) => return Ok(false),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        filled += read;
        if finder.find(&buf[..filled]).is_some() {
            return Ok(true);
        }
        let carry = carry_max.min(filled);
        buf.copy_within(filled - carry..filled, 0);
        filled = carry;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn stream_contains_finds_a_needle_split_across_reads() {
        // A reader that returns one byte per read puts every possible split
        // of the needle across two reads.
        struct OneByte<'a>(&'a [u8]);
        impl Read for OneByte<'_> {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                let Some((&first, rest)) = self.0.split_first() else {
                    return Ok(0);
                };
                out[0] = first;
                self.0 = rest;
                Ok(1)
            }
        }
        assert!(stream_contains(OneByte(b"xx needle yy"), b"needle").unwrap());
        assert!(!stream_contains(OneByte(b"xx needl e yy"), b"needle").unwrap());

        let mut big = vec![b'a'; STREAM_CHUNK * 3];
        let at = STREAM_CHUNK * 2 - 3;
        big[at..at + 6].copy_from_slice(b"needle");
        assert!(stream_contains(&big[..], b"needle").unwrap());
        assert!(!stream_contains(&big[..at], b"needle").unwrap());
        assert!(stream_contains(&b""[..], b"").unwrap());
    }

    #[test]
    fn stream_contains_stops_at_the_first_match() {
        // Reading past the match would hit the error, so a true answer
        // proves the scan stopped there.
        struct ThenFail(bool);
        impl Read for ThenFail {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                if self.0 {
                    return Err(io::Error::other("read past the match"));
                }
                self.0 = true;
                out[..6].copy_from_slice(b"needle");
                Ok(6)
            }
        }
        assert!(stream_contains(ThenFail(false), b"needle").unwrap());
    }
}
