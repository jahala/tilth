//! Readers that misbehave in controlled ways, for tests of streamed reads,
//! and a named pipe, for tests that a read never blocks on one.
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

/// Make a named pipe at `path`. Nothing writes to it, so opening it for
/// reading in the usual blocking way waits forever.
#[cfg(unix)]
pub fn make_pipe(path: &std::path::Path) {
    let made = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("mkfifo runs");
    assert!(made.success(), "mkfifo failed");
}

/// Run `f`, which must never block reading the pipe at `pipe`. Its result,
/// or `None` when it is still blocked after ten seconds. From then on this
/// keeps releasing the pipe until `f` returns, so the test fails instead of
/// hanging: holding the pipe open for writing lets a blocked open through,
/// and closing it again ends a blocked read.
#[cfg(unix)]
pub fn unless_blocked_on<T: Send>(
    pipe: &std::path::Path,
    f: impl FnOnce() -> T + Send,
) -> Option<T> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    let (sent, got) = std::sync::mpsc::channel();
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let result = f();
            done.store(true, Ordering::SeqCst);
            let _ = sent.send(result);
        });
        let result = got.recv_timeout(Duration::from_secs(10)).ok();
        while !done.load(Ordering::SeqCst) {
            // Opening a pipe for reading and writing never blocks.
            let writer = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(pipe);
            std::thread::sleep(Duration::from_millis(10));
            drop(writer);
            std::thread::sleep(Duration::from_millis(10));
        }
        result
    })
}
