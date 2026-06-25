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
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}
