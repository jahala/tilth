//! Reading source files for parsing and search.
//!
//! A file's bytes become text by one rule, [`into_text`]: a NUL byte in
//! the first [`BINARY_WINDOW`] bytes makes a file binary, and any other
//! file is text, each byte sequence that is not UTF-8 read as U+FFFD.
//! Search, symbols, outlines and reads all follow it, so a Latin-1 file is
//! searched, parsed and shown alike, and a binary one is skipped alike.
//! Only the hash-anchored writes read strictly, through [`read_to_string`]:
//! they write lines back, and must never turn a byte into U+FFFD. The
//! structural diff reads strictly too, until #312 rebuilds it.
//!
//! [`BINARY_WINDOW`]: crate::lang::detection::BINARY_WINDOW
//!
//! A UTF-8 byte-order mark (U+FEFF, bytes `EF BB BF`) is an encoding marker,
//! not text. Left in place it sits in front of line 1, so `str::trim_start`
//! keeps it (U+FEFF is not white space), a line-1 `#include` or `import`
//! stops being one, `serde_json` refuses the file, and search output prints
//! a stray invisible character. Every read that parses or searches a file
//! goes through here and drops the mark. Views that print hash anchors keep
//! the file's bytes as they are, because `tilth_write` verifies its anchors
//! against the file on disk.
//!
//! Every read of a file that a caller names or a walk finds, in this crate
//! and in the `tilth` binary, opens it through [`OpenFile`], so none waits
//! on a pipe.

use std::fs::{File, Metadata};
use std::io::{self, Read, Seek};
use std::ops::{ControlFlow, Range};
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

/// The text of a file's `bytes`, or `None` when they are binary: the one
/// rule by which tilth reads a file as text. Bytes with a NUL in their
/// first [`BINARY_WINDOW`] bytes are binary, as [`is_binary`] says. Any
/// other bytes are text, and each sequence in them that is not UTF-8 reads
/// as U+FFFD, as [`String::from_utf8_lossy`] reads it, so a stray byte
/// costs its own character and nothing else. Text that is UTF-8 becomes
/// the `String` without a copy.
///
/// [`BINARY_WINDOW`]: crate::lang::detection::BINARY_WINDOW
/// [`is_binary`]: crate::lang::detection::is_binary
#[must_use]
pub fn into_text(bytes: Vec<u8>) -> Option<String> {
    if crate::lang::detection::is_binary(&bytes) {
        return None;
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "by design: the home of the text rule, keeping UTF-8 without a copy"
    )]
    let text = String::from_utf8(bytes);
    Some(text.unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
}

/// The text of the file at `path` for parsing or search, by the rule of
/// [`into_text`] and without its byte-order mark, or `None` for a binary
/// file. It reads as [`read`] does, so a pipe, socket or device reads as
/// empty text.
///
/// # Errors
///
/// Returns the open's or the read's I/O error.
pub fn read_source(path: &Path) -> io::Result<Option<String>> {
    let Some(mut text) = into_text(read(path)?) else {
        return Ok(None);
    };
    if text.starts_with(BOM) {
        text.drain(..BOM.len());
    }
    Ok(Some(text))
}

/// The bytes of the file at `path`, as [`std::fs::read`] gives them, but
/// opened through [`OpenFile`]: a pipe, socket or device reads as empty
/// and never blocks, and the read stops at the length the file had when
/// it was opened.
///
/// # Errors
///
/// Returns the open's or the read's I/O error.
pub fn read(path: &Path) -> io::Result<Vec<u8>> {
    OpenFile::open(path)?.map_or_else(|| Ok(Vec::new()), |mut file| file.read_all())
}

/// The text of the file at `path` when all of it is UTF-8, as
/// [`std::fs::read_to_string`] gives it, but opened through [`OpenFile`],
/// as [`read`] is. This strict read is for the hash-anchored writes alone,
/// which write a file's lines back; everything else reads text by the rule
/// of [`into_text`].
///
/// # Errors
///
/// Returns the open's or the read's I/O error, and an
/// [`io::ErrorKind::InvalidData`] error for text that is not UTF-8.
pub fn read_to_string(path: &Path) -> io::Result<String> {
    #[expect(
        clippy::disallowed_methods,
        reason = "by design: the one strict decode, for the writes that must keep every byte"
    )]
    String::from_utf8(read(path)?).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "stream did not contain valid UTF-8",
        )
    })
}

/// A file open for reading, with its handle's metadata. Every read of it
/// stops at the length that metadata gives, so a file that grows while it
/// is read is read as it was when it was opened.
///
/// This is the one way tilth opens a file to read it. A pipe, socket or
/// device has no fixed contents, so tilth treats it as empty and never
/// reads it: [`OpenFile::open`] gives `None` for one.
pub struct OpenFile {
    file: File,
    meta: Metadata,
}

impl OpenFile {
    /// Open `path` for reading, or `None` when it is a pipe, socket or
    /// device. On Unix the open does not block, so a pipe that has no
    /// writer is found out from the handle's own metadata instead of
    /// hanging the open, and nothing can swap one in between a look at the
    /// path and the open. A regular file's handle then blocks as usual. A
    /// directory opens, and reading it fails.
    ///
    /// # Errors
    ///
    /// Returns the open's I/O error.
    pub fn open(path: &Path) -> io::Result<Option<Self>> {
        let file = open_for_reading(path)?;
        let meta = file.metadata()?;
        if meta.is_dir() {
            return Ok(Some(Self { file, meta }));
        }
        if !meta.is_file() {
            return Ok(None);
        }
        #[cfg(unix)]
        clear_nonblocking(&file)?;
        Ok(Some(Self { file, meta }))
    }

    /// The file's length when it was opened.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.meta.len()
    }

    /// Whether the file was empty when it was opened.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The metadata of the open handle.
    #[must_use]
    pub fn metadata(&self) -> &Metadata {
        &self.meta
    }

    /// The file from its start, up to its length at open.
    ///
    /// # Errors
    ///
    /// Returns the seek's I/O error.
    pub fn reader(&mut self) -> io::Result<io::Take<&mut File>> {
        self.file.rewind()?;
        Ok((&mut self.file).take(self.meta.len()))
    }

    /// All of the file, up to its length at open.
    ///
    /// # Errors
    ///
    /// Returns the read's I/O error.
    pub fn read_all(&mut self) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(self.capacity());
        self.reader()?.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    fn capacity(&self) -> usize {
        usize::try_from(self.len()).unwrap_or(0)
    }
}

/// Open `path` read-only. On Unix the open never waits for a pipe's writer.
#[cfg(unix)]
fn open_for_reading(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC;
    Ok(File::from(rustix::fs::open(path, flags, Mode::empty())?))
}

#[cfg(not(unix))]
#[expect(
    clippy::disallowed_methods,
    reason = "by design: the one open path itself, where no pipe can block the open"
)]
fn open_for_reading(path: &Path) -> io::Result<File> {
    File::open(path)
}

/// Make reads of `file` block again, as they do on a handle opened the
/// usual way. `O_NONBLOCK` only kept the open from waiting on a pipe.
#[cfg(unix)]
fn clear_nonblocking(file: &File) -> io::Result<()> {
    use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
    let flags = fcntl_getfl(file)?;
    fcntl_setfl(file, flags.difference(OFlags::NONBLOCK))?;
    Ok(())
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

/// `text` holds `word` as a whole identifier: no ASCII letter, digit, `_`
/// or `$` joins it on a side where `word` itself ends in one. `renew` is no
/// mention of `new`, and `$new` none in JavaScript either. Any other byte,
/// a non-ASCII one included, is a boundary. An empty word is in no text.
#[must_use]
pub fn mentions(text: &[u8], word: &str) -> bool {
    find_mention(text, 0, word.as_bytes()).is_some()
}

/// Does the file at `path` mention one of `words`, as [`mentions`] reads
/// it? It is how search, callers, grok and deps tell whether a file they
/// skip for size holds what they look for, and it streams the file
/// through [`file_holds`], so a file of any size costs a few chunks of
/// memory.
#[must_use]
pub fn file_mentions(path: &Path, words: &[&str]) -> bool {
    let longest = words.iter().map(|w| w.len()).max().unwrap_or(0);
    file_holds(path, longest, |window, from| {
        words
            .iter()
            .filter_map(|w| find_mention(window, from, w.as_bytes()))
            .min_by_key(|hit| (hit.start, hit.end))
    })
}

/// Does the file at `path` hold a hit of `find`? The file is opened
/// through [`OpenFile`] and streamed in bounded windows; see `holds` for
/// what `find` and `overlap` mean. A file that cannot be read holds nothing.
pub fn file_holds(
    path: &Path,
    overlap: usize,
    find: impl FnMut(&[u8], usize) -> Option<Range<usize>>,
) -> bool {
    let Ok(Some(mut file)) = OpenFile::open(path) else {
        return false;
    };
    file.reader()
        .and_then(|reader| holds(reader, overlap, find))
        .unwrap_or(false)
}

/// Bytes of context a finder may look at on each side of a hit: one UTF-8
/// character, enough for a word boundary, `^` or `$`.
const EDGE: usize = 4;

/// Does `reader` hold a hit of `find`? Binary input holds nothing: a NUL
/// byte in its first [`BINARY_WINDOW`] bytes, the rule every read uses.
///
/// The input streams through a window of one chunk plus the last
/// `overlap + 2 * EDGE` bytes of the window before, so the check holds that
/// much memory whatever the input's size or line length, and it stops at
/// the first hit. `find(window, from)` returns the leftmost hit that starts
/// at or after `from`, and may look `EDGE` bytes past either end of it. A
/// hit is trusted only when the window holds that context, or the input's
/// own end; a hit no longer than `overlap` is always judged whole in some
/// window. When the leftmost hit runs past the window, the window is left
/// for the next one, so a longer hit across a cut, or a shorter one after
/// it, may go unseen.
///
/// [`BINARY_WINDOW`]: crate::lang::detection::BINARY_WINDOW
///
/// # Errors
///
/// Returns the reader's I/O error.
fn holds(
    reader: impl Read,
    overlap: usize,
    mut find: impl FnMut(&[u8], usize) -> Option<Range<usize>>,
) -> io::Result<bool> {
    let carry = overlap + 2 * EDGE;
    let mut buf = vec![0; STREAM_CHUNK];
    let mut window = Vec::with_capacity(carry + STREAM_CHUNK);
    // Whether `window` starts at the input's first byte.
    let mut at_start = true;
    let mut verdict = None;
    read_chunks(reader, &mut buf, |chunk| {
        window.extend_from_slice(chunk);
        if at_start && window.len() < crate::lang::detection::BINARY_WINDOW {
            return ControlFlow::Continue(());
        }
        verdict = judge(&window, at_start, false, &mut find);
        if verdict.is_some() {
            return ControlFlow::Break(());
        }
        if window.len() > carry {
            window.drain(..window.len() - carry);
            at_start = false;
        }
        ControlFlow::Continue(())
    })?;
    Ok(verdict
        .or_else(|| judge(&window, at_start, true, &mut find))
        .unwrap_or(false))
}

/// What one window of [`holds`] settles: `Some(true)` for a trusted hit,
/// `Some(false)` for binary input, `None` when the input's rest decides.
fn judge(
    window: &[u8],
    at_start: bool,
    at_end: bool,
    find: &mut impl FnMut(&[u8], usize) -> Option<Range<usize>>,
) -> Option<bool> {
    if at_start && crate::lang::detection::is_binary(window) {
        return Some(false);
    }
    // A hit that starts in a later window's first `EDGE` bytes was judged
    // whole, with its context, in the window before.
    let from = if at_start { 0 } else { EDGE };
    let hit = find(window, from)?;
    (at_end || hit.end + EDGE <= window.len()).then_some(true)
}

/// The first mention of `word` in `text` that starts at or after `from`,
/// as [`mentions`] reads one. A byte before `from` still counts as context.
fn find_mention(text: &[u8], from: usize, word: &[u8]) -> Option<Range<usize>> {
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let (&first, &last) = (word.first()?, word.last()?);
    memchr::memmem::find_iter(text.get(from..)?, word)
        .map(|at| from + at..from + at + word.len())
        .find(|hit| {
            let joined_before =
                is_ident(first) && hit.start.checked_sub(1).is_some_and(|i| is_ident(text[i]));
            let joined_after = is_ident(last) && text.get(hit.end).copied().is_some_and(is_ident);
            !joined_before && !joined_after
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mention_in(reader: impl Read, word: &str) -> bool {
        holds(reader, word.len(), |w, from| {
            find_mention(w, from, word.as_bytes())
        })
        .unwrap()
    }

    #[test]
    fn a_mention_is_a_whole_identifier() {
        for (text, holds) in [
            ("needle", true),
            ("a needle;", true),
            ("(needle)", true),
            ("\u{2192}needle\u{2190}", true),
            ("xneedle", false),
            ("needlex", false),
            ("needle_v2", false),
            ("$needle", false),
            ("needle_v2 needle", true),
            ("needle_v2 xneedle", false),
        ] {
            assert_eq!(mentions(text.as_bytes(), "needle"), holds, "{text:?}");
        }
        assert!(!mentions(b"anything", ""));
        // A side where the word ends in no identifier byte needs no boundary.
        assert!(mentions(b"x@Override", "@Override"));
    }

    /// The check stops at the first hit. These readers never end, so a
    /// check that read the whole input would never return.
    #[test]
    fn the_check_stops_at_the_first_hit_on_endless_input() {
        let lines = io::Cursor::new(b"first line\nneedle here\n".to_vec()).chain(io::repeat(b'\n'));
        assert!(mention_in(lines, "needle"));
        let one_line = io::Cursor::new(b"var a=needle;".to_vec()).chain(io::repeat(b'a'));
        assert!(mention_in(one_line, "needle"));
    }

    /// A NUL byte in the first 512 bytes makes the input binary, which
    /// holds nothing, even when the reads hand it over a byte at a time. One
    /// further on is a stray byte in a text file.
    #[test]
    fn only_a_nul_near_the_start_makes_the_input_binary() {
        use crate::test_readers::OneByte;
        assert!(!mention_in(&b"needle\0"[..], "needle"));
        assert!(!mention_in(OneByte(b"needle header\0"), "needle"));
        let mut late = vec![b'x'; 600];
        late.extend_from_slice(b"\0 needle\n");
        assert!(mention_in(&late[..], "needle"));
    }

    /// Every cut between windows, whatever bytes it falls between, and
    /// reads of any size give the answer [`mentions`] gives on the whole
    /// input.
    #[test]
    fn a_cut_between_windows_changes_no_answer() {
        use crate::test_readers::OneByte;
        for text in [
            "needle",
            "xneedle",
            "needlex",
            "needle_v2 needle",
            "needle_v2 xneedle",
        ] {
            let input = |at: usize, room: usize| {
                let mut input = vec![b' '; at];
                input.extend_from_slice(text.as_bytes());
                input.resize(at + text.len() + room, b' ');
                input
            };
            let whole = mentions(text.as_bytes(), "needle");
            // Reads of a byte cut after every byte once the first 512 are in.
            for at in (0..4).chain(500..530) {
                let input = input(at, 8);
                assert_eq!(
                    mention_in(OneByte(&input), "needle"),
                    whole,
                    "{text:?} at {at}"
                );
            }
            // Whole reads cut every `STREAM_CHUNK` bytes, given room past the text.
            for at in (1..=3).flat_map(|k| k * STREAM_CHUNK - 20..=k * STREAM_CHUNK + 4) {
                let input = input(at, STREAM_CHUNK);
                assert_eq!(mention_in(&input[..], "needle"), whole, "{text:?} at {at}");
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
        assert_eq!(read_source(&path).unwrap().as_deref(), Some("use a;\n"));
    }

    /// A NUL byte in the first 512 bytes makes bytes binary; any other
    /// bytes are text, each sequence that is not UTF-8 read as U+FFFD.
    #[test]
    fn bytes_are_binary_or_lossy_text() {
        assert_eq!(
            into_text(b"caf\xc3\xa9\n".to_vec()).as_deref(),
            Some("caf\u{e9}\n")
        );
        assert_eq!(
            into_text(b"caf\xe9 \xe2\x82 ok\n".to_vec()).as_deref(),
            Some("caf\u{fffd} \u{fffd} ok\n")
        );
        assert_eq!(into_text(b"text\0".to_vec()), None);
        let late = format!("{}\0", "x".repeat(512));
        assert_eq!(into_text(late.clone().into_bytes()), Some(late));
        assert_eq!(into_text(Vec::new()).as_deref(), Some(""));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.c");
        std::fs::write(&path, b"\xef\xbb\xbfint caf\xe9;\n").unwrap();
        assert_eq!(
            read_source(&path).unwrap().as_deref(),
            Some("int caf\u{fffd};\n")
        );
        std::fs::write(&path, b"int a;\0\n").unwrap();
        assert_eq!(read_source(&path).unwrap(), None);
    }

    /// A regular file's handle blocks as a usual one does: the open's
    /// `O_NONBLOCK` is cleared once the handle is known to be a file.
    #[cfg(unix)]
    #[test]
    fn a_regular_files_handle_blocks_as_usual() {
        use rustix::fs::{fcntl_getfl, OFlags};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "fn a() {}\n").unwrap();
        let file = OpenFile::open(&path).unwrap().unwrap();
        let flags = fcntl_getfl(&file.file).unwrap();
        assert!(!flags.contains(OFlags::NONBLOCK), "{flags:?}");
    }

    /// A pipe, socket or device reads as empty, and no read blocks on a
    /// pipe that has no writer.
    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "by design: the strict read is one of the reads this checks"
    )]
    fn a_pipe_or_device_reads_as_empty() {
        use crate::test_readers::{make_pipe, unless_blocked_on};
        let dir = tempfile::tempdir().unwrap();
        let pipe = dir.path().join("pipe.rs");
        make_pipe(&pipe);
        for path in [pipe.as_path(), Path::new("/dev/null")] {
            let reads = unless_blocked_on(&pipe, || {
                (
                    OpenFile::open(path).unwrap().is_none(),
                    read(path).unwrap(),
                    read_to_string(path).unwrap(),
                    read_source(path).unwrap(),
                )
            })
            .expect("a read blocked on the pipe");
            assert_eq!(
                reads,
                (true, Vec::new(), String::new(), Some(String::new()))
            );
        }
    }

    /// A file that grows while it is read is read as it was when opened.
    #[test]
    fn a_whole_read_stops_at_the_length_at_open() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("growing.log");
        std::fs::write(&path, "first\n").unwrap();
        let mut file = OpenFile::open(&path).unwrap().unwrap();
        let mut appender = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        appender.write_all(b"second\n").unwrap();
        assert_eq!(file.read_all().unwrap(), b"first\n");
    }

    #[test]
    fn a_finder_finds_a_needle_split_across_reads() {
        use crate::test_readers::OneByte;
        let mut finder = StreamFinder::new(b"needle");
        assert!(finder.contains(OneByte(b"xx needle yy")).unwrap());
        assert!(!finder.contains(OneByte(b"xx needl e yy")).unwrap());

        let mut big = vec![b'a'; STREAM_CHUNK * 3];
        let at = STREAM_CHUNK * 2 - 3;
        big[at..at + 6].copy_from_slice(b"needle");
        assert!(finder.contains(&big[..]).unwrap());
        assert!(!finder.contains(&big[..at]).unwrap());
        assert!(StreamFinder::new(b"").contains(&b""[..]).unwrap());
    }

    #[test]
    fn a_finder_stops_at_the_first_match() {
        // Reading past the match would hit the error, so a true answer
        // proves the scan stopped there.
        let reader = crate::test_readers::ThenFail::new(b"needle");
        assert!(StreamFinder::new(b"needle").contains(reader).unwrap());
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
