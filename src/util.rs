//! Shared utilities used by both `edit` and `install`.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Whether a guarded write reached the target, or refused because the target
/// no longer held the content the caller verified.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WriteOutcome {
    Written,
    Stale,
}

/// [`atomic_write_bytes`], but the rename only happens while `path` still
/// holds `expected` byte for byte. A caller that verified content, computed a
/// replacement, and is about to publish it uses this so a write that landed in
/// between is refused instead of silently overwritten.
///
/// The check runs after the temp file is written, so the window it leaves is
/// one read plus one rename rather than the caller's whole verify-and-compute
/// pass. That window is not zero: this narrows cross-process lost updates, it
/// does not eliminate them.
pub(crate) fn atomic_write_bytes_if_unchanged(
    path: &Path,
    bytes: &[u8],
    expected: &[u8],
) -> std::io::Result<WriteOutcome> {
    write_via_temp(path, bytes, Some(expected))
}

/// Write `bytes` to `path` atomically: write to a temp file in the same
/// directory, preserve the original file's permissions (if it exists), then
/// rename into place. A crash mid-write leaves the original intact, and a
/// reader holding an mmap of the original keeps a valid mapping — the inode is
/// replaced, never truncated.
///
/// The temp name is qualified with the process ID and a process-wide counter
/// so concurrent or batched writes in the same directory can't collide.
pub(crate) fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_via_temp(path, bytes, None).map(drop)
}

fn write_via_temp(
    path: &Path,
    bytes: &[u8],
    expected: Option<&[u8]>,
) -> std::io::Result<WriteOutcome> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    // `Path::new("foo.txt").parent()` returns `Some("")`, not `None`; treat an
    // empty parent as "no directory" so the temp file anchors to "." rather than
    // the empty-string path.
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(".tilth-tmp.{}.{n}", std::process::id()));
    std::fs::write(&tmp, bytes).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    // Preserve original file permissions so the rename doesn't widen or strip
    // the mode. Ignore errors — target may not exist yet or platform may not
    // support it; the write already succeeded.
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    // Last look before publishing: a mismatch means someone else wrote the
    // file after the caller verified it, so drop the temp and report staleness
    // rather than clobbering their write. An unreadable target counts as
    // changed — the file the caller verified is no longer there to write over.
    if let Some(expected) = expected {
        if !std::fs::read(path).is_ok_and(|current| current == expected) {
            let _ = std::fs::remove_file(&tmp);
            return Ok(WriteOutcome::Stale);
        }
    }
    std::fs::rename(&tmp, path)
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
        .map(|()| WriteOutcome::Written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_siblings(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".tilth-tmp."))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn guarded_write_publishes_when_content_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        std::fs::write(&p, "old\n").unwrap();

        let outcome = atomic_write_bytes_if_unchanged(&p, b"new\n", b"old\n").unwrap();
        assert_eq!(outcome, WriteOutcome::Written);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new\n");
        assert!(
            temp_siblings(dir.path()).is_empty(),
            "temp file left behind"
        );
    }

    #[test]
    fn guarded_write_refuses_when_content_changed_underneath() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        // What another writer left behind after this caller read "old\n".
        std::fs::write(&p, "someone else's edit\n").unwrap();

        let outcome = atomic_write_bytes_if_unchanged(&p, b"new\n", b"old\n").unwrap();
        assert_eq!(outcome, WriteOutcome::Stale);
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "someone else's edit\n",
            "a refused write must not touch the file"
        );
        assert!(
            temp_siblings(dir.path()).is_empty(),
            "temp file left behind"
        );
    }

    #[test]
    fn guarded_write_refuses_when_target_disappeared() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("gone.txt");

        let outcome = atomic_write_bytes_if_unchanged(&p, b"new\n", b"old\n").unwrap();
        assert_eq!(outcome, WriteOutcome::Stale);
        assert!(!p.exists(), "a refused write must not create the file");
        assert!(
            temp_siblings(dir.path()).is_empty(),
            "temp file left behind"
        );
    }
}
