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
//!
//! Every read of a file that a caller names or a walk finds, in this crate
//! and in the `tilth` binary, opens it through [`OpenFile`], so none waits
//! on a pipe.

use std::fs::{File, Metadata};
use std::io::{self, Read, Seek};
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
/// It reads as [`read_to_string`] does, so a pipe, socket or device reads
/// as empty.
///
/// # Errors
///
/// Returns the open's or the read's I/O error, and an
/// [`io::ErrorKind::InvalidData`] error for text that is not UTF-8.
pub fn read_source(path: &Path) -> io::Result<String> {
    let mut text = read_to_string(path)?;
    if text.starts_with(BOM) {
        text.drain(..BOM.len());
    }
    Ok(text)
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
    match OpenFile::open(path)? {
        Some(mut file) => file.read_all(),
        None => Ok(Vec::new()),
    }
}

/// The text of the file at `path`, as [`std::fs::read_to_string`] gives
/// it, but opened through [`OpenFile`], as [`read`] is.
///
/// # Errors
///
/// Returns the open's or the read's I/O error, and an
/// [`io::ErrorKind::InvalidData`] error for text that is not UTF-8.
pub fn read_to_string(path: &Path) -> io::Result<String> {
    match OpenFile::open(path)? {
        Some(mut file) => file.read_to_string(),
        None => Ok(String::new()),
    }
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

    /// All of the file as text, up to its length at open.
    ///
    /// # Errors
    ///
    /// Returns the read's I/O error, and an [`io::ErrorKind::InvalidData`]
    /// error for text that is not UTF-8.
    pub fn read_to_string(&mut self) -> io::Result<String> {
        let mut text = String::with_capacity(self.capacity());
        self.reader()?.read_to_string(&mut text)?;
        Ok(text)
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
            assert_eq!(reads, (true, Vec::new(), String::new(), String::new()));
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
        assert_eq!(file.read_to_string().unwrap(), "first\n");
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
