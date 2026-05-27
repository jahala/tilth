//! `tilth_write` file-mode helpers: `overwrite` and `append`.
//!
//! `overwrite` is **create-only by default** — an atomic `O_CREAT|O_EXCL`
//! open fails with `ErrorKind::AlreadyExists` if the path already exists
//! (regular file *or* dangling symlink), so there is no TOCTOU window and no
//! silent clobber. Pass `overwrite = true` to replace an existing file. The
//! rewrite refuses to follow symlinks (live or dangling): on Unix the open
//! passes `O_NOFOLLOW`, so the kernel returns `ELOOP` rather than resolving
//! the link and writing the target — closing the scope-escape at the syscall
//! layer. `ELOOP` is remapped to `ErrorKind::InvalidInput`. On non-Unix the
//! rewrite falls back to `fs::write` (Windows symlink semantics differ; no
//! analogous escape).

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

#[cfg(unix)]
fn rewrite_existing(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;

    let mut f = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| {
            if e.raw_os_error() == Some(libc::ELOOP) {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "refusing to overwrite through symlink",
                )
            } else {
                e
            }
        })?;
    f.write_all(content.as_bytes())
}

#[cfg(not(unix))]
fn rewrite_existing(path: &Path, content: &str) -> std::io::Result<()> {
    fs::write(path, content)
}

/// Append `content` to `path`, creating the file (and parent dirs) if absent.
pub fn write_append(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(p) = path.parent() {
        if !p.as_os_str().is_empty() {
            fs::create_dir_all(p)?;
        }
    }
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(content.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
