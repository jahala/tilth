//! Shared utilities used by both `edit` and `install`.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Write `bytes` to `path` atomically: write to a temp file in the same
/// directory, preserve the original file's permissions (if it exists), then
/// rename into place. A crash mid-write leaves the original intact.
///
/// The temp name is qualified with the process ID and a process-wide counter
/// so concurrent or batched writes in the same directory can't collide.
pub(crate) fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    // Path::new("foo.txt").parent() returns Some(""), not None; filter it so
    // we fall through to the "." default and document intent explicitly.
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
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Create `path` from `bytes` without replacing an existing destination, and
/// without ever exposing a partially-written file at that path.
///
/// Stage into a same-directory temp opened `O_CREAT|O_EXCL` — exclusive
/// creation is what makes the staging write safe, since a symlink planted at
/// the predictable temp name is refused rather than followed — then commit with
/// `hard_link`, which fails on an existing destination instead of replacing it.
/// Both guarantees hold: a failed or interrupted write leaves the destination
/// absent, and an existing destination is never clobbered.
pub(crate) fn atomic_create_bytes_no_replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(".tilth-create-tmp.{}.{n}", std::process::id()));

    let staged = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| f.write_all(bytes));
    if let Err(e) = staged {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    let result = std::fs::hard_link(&tmp, path);
    let _ = std::fs::remove_file(&tmp);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The destination must never appear unless it appears complete. A create
    /// that fails has to leave nothing behind, or the agent's retry hits
    /// "target already exists" for a file tilth half-created.
    #[test]
    fn failed_create_leaves_no_destination_and_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        // A directory at the target makes hard_link fail at the commit step,
        // after staging has already written the bytes.
        let target = dir.path().join("occupied");
        std::fs::create_dir(&target).unwrap();

        assert!(atomic_create_bytes_no_replace(&target, b"payload").is_err());
        assert!(target.is_dir(), "must not have replaced the destination");

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".tilth-create-tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[test]
    fn create_writes_exact_bytes_and_refuses_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("new.rs");
        atomic_create_bytes_no_replace(&target, b"fn a() {}\n").expect("creates");
        assert_eq!(std::fs::read(&target).unwrap(), b"fn a() {}\n");

        let err = atomic_create_bytes_no_replace(&target, b"clobber").expect_err("no replace");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"fn a() {}\n",
            "existing content must survive"
        );
    }
}
