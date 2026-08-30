//! `tilth_write` file-mode helpers: `overwrite` and `append`.
//!
//! `overwrite` is **create-only by default** — an atomic `O_CREAT|O_EXCL`
//! open fails with `ErrorKind::AlreadyExists` if the path already exists
//! (regular file *or* dangling symlink), so there is no TOCTOU window and no
//! silent clobber. Pass `overwrite = true` to replace an existing file. The
//! replacement writes a sibling temp file and renames it over the target, so
//! the original inode is never truncated: a concurrent reader holding an
//! `Mmap` of it keeps a valid mapping instead of faulting with SIGBUS when
//! the file shrinks under it. Readers see either the whole old file or the
//! whole new one, never a prefix.
//!
//! The rewrite refuses to follow symlinks (live or dangling): on Unix an
//! `O_NOFOLLOW` open probes the target first, so the kernel returns `ELOOP`
//! rather than resolving the link — closing the scope-escape at the syscall
//! layer. `ELOOP` is remapped to `ErrorKind::InvalidInput`. That probe is a
//! separate syscall from the rename, so a symlink swapped into the path
//! afterwards is *replaced* rather than refused; containment still holds
//! because `rename(2)` never follows a symlink at its destination, so the
//! link's target is not written either way.
//!
//! `append` carries the same `O_NOFOLLOW` guard on Unix: appending through a
//! symlink (live or dangling) is refused with `ErrorKind::InvalidInput`, so an
//! in-scope symlink can't be used to append outside the scope. This keeps the
//! two write modes symmetric — neither follows a symlink. Non-Unix falls back
//! to a plain create+append open.

use std::fs;
use std::path::Path;

/// Write `content` to `path`, creating parent dirs if absent. Create-only
/// unless `overwrite` is true. See module docs for the symlink guarantees.
pub fn write_overwrite(path: &Path, content: &str, overwrite: bool) -> std::io::Result<()> {
    use std::io::Write as _;

    if let Some(p) = path.parent() {
        if !p.as_os_str().is_empty() {
            fs::create_dir_all(p)?;
        }
    }
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut f) => f.write_all(content.as_bytes()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && overwrite => {
            rewrite_existing(path, content)
        }
        Err(e) => Err(e),
    }
}

/// Remap a `O_NOFOLLOW` open's `ELOOP` (the path's final component is a
/// symlink) to a clear `InvalidInput` refusal; pass any other error through.
#[cfg(unix)]
fn refuse_symlink(e: std::io::Error, action: &str) -> std::io::Error {
    if e.raw_os_error() == Some(libc::ELOOP) {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("refusing to {action} through symlink"),
        )
    } else {
        e
    }
}

fn rewrite_existing(path: &Path, content: &str) -> std::io::Result<()> {
    refuse_symlink_target(path)?;
    crate::util::atomic_write_bytes(path, content.as_bytes())
}

/// Refuse when `path`'s final component is a symlink. The open neither
/// truncates nor writes — it exists only to make the kernel resolve
/// `O_NOFOLLOW` for us.
#[cfg(unix)]
fn refuse_symlink_target(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| refuse_symlink(e, "overwrite"))
        .map(drop)
}

#[cfg(not(unix))]
fn refuse_symlink_target(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Append `content` to `path`, creating the file (and parent dirs) if absent.
/// On Unix the open passes `O_NOFOLLOW`, so appending through a symlink (live
/// or dangling) is refused with `ErrorKind::InvalidInput` — symmetric with the
/// `overwrite` path, so neither mode can write through an in-scope symlink.
pub fn write_append(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(p) = path.parent() {
        if !p.as_os_str().is_empty() {
            fs::create_dir_all(p)?;
        }
    }
    let mut f = open_append(path)?;
    f.write_all(content.as_bytes())
}

#[cfg(unix)]
fn open_append(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| refuse_symlink(e, "append"))
}

#[cfg(not(unix))]
fn open_append(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new().create(true).append(true).open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reader/writer race for [`overwrite_never_faults_an_mmap_reader`]. Runs
    /// in a re-executed child because the fault it guards against kills the
    /// process rather than failing an assertion.
    fn mmap_race_body() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("foundation.py");
        let line = "def f(): return 1  # ..........................................\n";
        let content: String = line.repeat(365);
        std::fs::write(&p, &content).unwrap();

        let stop = AtomicBool::new(false);
        let cache = crate::cache::OutlineCache::new();
        std::thread::scope(|s| {
            s.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    let _ = write_overwrite(&p, &content, true);
                }
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            while std::time::Instant::now() < deadline {
                let _ = crate::read::read_file(&p, None, true, &cache, false);
            }
            stop.store(true, Ordering::Relaxed);
        });
    }

    /// `tilth_read` holds an `Mmap` of a file while `tilth_write` replaces it.
    /// An in-place `O_TRUNC` rewrite shrinks the inode under the live mapping,
    /// so the reader's next page touch faults with SIGBUS and takes the whole
    /// MCP process down. Replacing by rename leaves the reader's inode intact.
    ///
    /// The fault is a signal, not a panic, so the race runs in a re-executed
    /// child of this test binary and the parent asserts on its exit status.
    #[test]
    fn overwrite_never_faults_an_mmap_reader() {
        const CHILD_ENV: &str = "TILTH_MMAP_RACE_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            mmap_race_body();
            return;
        }
        let out = std::process::Command::new(
            std::env::current_exe().expect("test binary path is readable"),
        )
        .args([
            "mcp::write::tests::overwrite_never_faults_an_mmap_reader",
            "--exact",
        ])
        .env(CHILD_ENV, "1")
        .output()
        .expect("re-exec of the test binary");
        assert!(
            out.status.success(),
            "a concurrent overwrite faulted an mmap reader: {}",
            out.status
        );
        // A filter that matches nothing also exits 0, which would make this
        // test pass without ever running the race.
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "the child ran no test — the name filter matched nothing"
        );
    }

    /// A concurrent reader must never observe a half-written file. The
    /// in-place `O_TRUNC` rewrite this replaced exposed a zero-length window
    /// on every overwrite — the same window that faults an mmap reader.
    /// Setup writes go through the atomic primitive so any partial read the
    /// assertion catches can only come from `write_overwrite` itself.
    #[test]
    fn overwrite_is_never_observable_as_partial() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("racy.txt");
        let before: String = "old\n".repeat(40_000);
        let after: String = "new!\n".repeat(40_000);
        std::fs::write(&p, &before).unwrap();

        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| {
                for _ in 0..200 {
                    write_overwrite(&p, &after, true).unwrap();
                    crate::util::atomic_write_bytes(&p, before.as_bytes()).unwrap();
                }
                stop.store(true, Ordering::Relaxed);
            });
            while !stop.load(Ordering::Relaxed) {
                let seen = std::fs::read_to_string(&p).expect("target file is always linked");
                assert!(
                    seen == before || seen == after,
                    "reader observed a partially written file ({} bytes)",
                    seen.len()
                );
            }
        });
    }

    #[test]
    fn write_overwrite_creates_new_file_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("new/nested/file.txt");
        write_overwrite(&p, "hello\n", false).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello\n");
    }

    #[test]
    fn write_overwrite_empty_content_touches() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("touch.txt");
        write_overwrite(&p, "", false).unwrap();
        assert!(p.exists());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "");
    }

    #[test]
    fn write_overwrite_create_only_fails_on_existing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("exists.txt");
        std::fs::write(&p, "original").unwrap();
        let err = write_overwrite(&p, "new", false).expect_err("expected AlreadyExists");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "original",
            "create-only must not clobber"
        );
    }

    #[test]
    fn write_overwrite_with_overwrite_flag_clobbers() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("exists.txt");
        std::fs::write(&p, "original").unwrap();
        write_overwrite(&p, "replaced", true).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "replaced");
    }

    #[cfg(unix)]
    #[test]
    fn write_overwrite_create_only_refuses_dangling_symlink() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link.txt");
        symlink(dir.path().join("missing-target"), &link).unwrap();
        let err = write_overwrite(&link, "x", false).expect_err("dangling symlink → AlreadyExists");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[cfg(unix)]
    #[test]
    fn write_overwrite_with_overwrite_flag_refuses_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        // Dangling symlink: fs::write would create the target.
        let dangling = dir.path().join("dangling.txt");
        symlink(dir.path().join("missing-target"), &dangling).unwrap();
        let err = write_overwrite(&dangling, "x", true)
            .expect_err("overwrite=true through dangling symlink must fail");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        // Live symlink: fs::write would clobber the target through the link.
        let target = dir.path().join("real.txt");
        std::fs::write(&target, "real").unwrap();
        let link = dir.path().join("link.txt");
        symlink(&target, &link).unwrap();
        let err = write_overwrite(&link, "x", true)
            .expect_err("overwrite=true through live symlink must fail");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "real",
            "symlink target must be untouched"
        );
    }

    #[test]
    fn write_append_creates_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("log/app.log");
        write_append(&p, "line1\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "line1\n");
    }

    #[test]
    fn write_append_extends_existing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("app.log");
        std::fs::write(&p, "line1\n").unwrap();
        write_append(&p, "line2\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "line1\nline2\n");
    }

    #[cfg(unix)]
    #[test]
    fn write_append_refuses_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        // Dangling symlink: a plain create+append would create the target.
        let dangling = dir.path().join("dangling.log");
        symlink(dir.path().join("missing-target"), &dangling).unwrap();
        let err =
            write_append(&dangling, "x\n").expect_err("append through dangling symlink must fail");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        // Live symlink: a plain create+append would write the target through the link.
        let target = dir.path().join("real.log");
        std::fs::write(&target, "real\n").unwrap();
        let link = dir.path().join("link.log");
        symlink(&target, &link).unwrap();
        let err = write_append(&link, "x\n").expect_err("append through live symlink must fail");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "real\n",
            "symlink target must be untouched"
        );
    }
}
