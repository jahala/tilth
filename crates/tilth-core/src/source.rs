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
use std::ops::ControlFlow;
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

/// How many bytes a streamed read hands over at a time. A buffer of this
/// size is all the memory a streamed read needs, whatever the input's size.
pub const STREAM_CHUNK: usize = 64 * 1024;

/// Read `reader` into `buf` one chunk at a time and hand each chunk to
/// `each`, until the input ends or `each` returns [`ControlFlow::Break`].
/// This is the one streamed-read loop: line counting, section reads and
/// substring scans all run on it. An interrupted read is retried.
///
/// # Errors
///
/// Returns the reader's I/O error.
pub fn read_chunks(
    mut reader: impl Read,
    buf: &mut [u8],
    mut each: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> io::Result<()> {
    loop {
        let read = match reader.read(buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if each(&buf[..read]).is_break() {
            return Ok(());
        }
    }
}

/// Finds one fixed byte string in streamed inputs. It keeps its buffers
/// between inputs, so a walk that checks thousands of files makes one of
/// these per thread instead of allocating per file.
pub struct StreamFinder {
    finder: memchr::memmem::Finder<'static>,
    buf: Vec<u8>,
    /// The last `needle.len() - 1` bytes seen, where a match that continues
    /// into the next chunk can start.
    tail: Vec<u8>,
    /// `tail` followed by the start of the next chunk: the only bytes a match
    /// spanning two chunks can occupy.
    seam: Vec<u8>,
}

impl StreamFinder {
    /// A finder for `needle`.
    #[must_use]
    pub fn new(needle: &[u8]) -> Self {
        Self {
            finder: memchr::memmem::Finder::new(needle).into_owned(),
            buf: vec![0; STREAM_CHUNK],
            tail: Vec::new(),
            seam: Vec::new(),
        }
    }

    /// Whether `reader` holds the needle anywhere. It stops at the first
    /// match, and a match split across two reads is still found. An empty
    /// needle is in every input.
    ///
    /// # Errors
    ///
    /// Returns the reader's I/O error.
    pub fn contains(&mut self, reader: impl Read) -> io::Result<bool> {
        let overlap = self.finder.needle().len().saturating_sub(1);
        if self.finder.needle().is_empty() {
            return Ok(true);
        }
        let Self {
            finder,
            buf,
            tail,
            seam,
        } = self;
        tail.clear();
        let mut found = false;
        read_chunks(reader, buf, |chunk| {
            seam.clear();
            seam.extend_from_slice(tail);
            seam.extend_from_slice(&chunk[..chunk.len().min(overlap)]);
            if finder.find(seam).is_some() || finder.find(chunk).is_some() {
                found = true;
                return ControlFlow::Break(());
            }
            tail.extend_from_slice(&chunk[chunk.len().saturating_sub(overlap)..]);
            let excess = tail.len().saturating_sub(overlap);
            tail.drain(..excess);
            ControlFlow::Continue(())
        })?;
        Ok(found)
    }
}

/// Whether `reader` holds `needle`: a one-off [`StreamFinder`]. A caller
/// that checks many inputs keeps a `StreamFinder` instead.
///
/// # Errors
///
/// Returns the reader's I/O error.
pub fn stream_contains(reader: impl Read, needle: &[u8]) -> io::Result<bool> {
    StreamFinder::new(needle).contains(reader)
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

    #[test]
    fn a_reused_finder_starts_each_input_fresh() {
        // The tail of one input must never join the head of the next: a file
        // ending in "nee" and one starting with "dle" do not make "needle".
        let mut finder = StreamFinder::new(b"needle");
        assert!(!finder.contains(&b"xx nee"[..]).unwrap());
        assert!(!finder.contains(&b"dle xx"[..]).unwrap());
        assert!(finder.contains(&b"a needle"[..]).unwrap());
        assert!(!finder.contains(&b"none"[..]).unwrap());
    }
}
