//! Readers that misbehave in controlled ways, for tests of streamed reads.
//! Shared by this crate's tests and, through the `test-fixtures` feature,
//! by the `tilth` binary's.

use std::io::{self, Read};

/// Hands over one byte per read, so a test puts every possible split of
/// its input across two reads.
pub struct OneByte<'a>(pub &'a [u8]);

impl Read for OneByte<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let Some((&first, rest)) = self.0.split_first() else {
            return Ok(0);
        };
        let Some(slot) = out.first_mut() else {
            return Ok(0);
        };
        *slot = first;
        self.0 = rest;
        Ok(1)
    }
}

/// Hands over `chunk` in one read, then fails every read after it. A
/// streamed read that succeeds on it proves it never read past `chunk`.
pub struct ThenFail {
    chunk: &'static [u8],
    served: bool,
}

impl ThenFail {
    /// A reader that serves `chunk` once and then fails.
    #[must_use]
    pub const fn new(chunk: &'static [u8]) -> Self {
        Self {
            chunk,
            served: false,
        }
    }
}

impl Read for ThenFail {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.served {
            return Err(io::Error::other("read past the end of the chunk"));
        }
        self.served = true;
        let len = self.chunk.len().min(out.len());
        out[..len].copy_from_slice(&self.chunk[..len]);
        Ok(len)
    }
}
