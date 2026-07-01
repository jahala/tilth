//! `tilth model pull` — copy or download a model into the tilth model cache.
//!
//! Usage:
//!   tilth model pull --name reranker --source /path/to/dir
//!   tilth model pull --name reranker --source <https://example.com/models/reranker>
//!
//! Copies `model.onnx` + `tokenizer.json` from a local directory, or
//! downloads them from an HTTP(S) URL prefix, into
//! `~/.cache/tilth/models/<name>/`.
//!
//! HTTP download is only available when the `model-pull-http` feature is
//! enabled. The default build is local-copy only (zero new dependency weight).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::infer::ModelConfig;

/// Error type for model-pull operations.
#[derive(Debug)]
pub enum PullError {
    /// The destination directory could not be created.
    Io(io::Error),
    /// The source path does not exist or the required files are missing.
    SourceMissing(String),
    /// HTTP download is not available in this build.
    HttpUnavailable,
    /// An HTTP download failed.
    HttpError(String),
}

impl std::fmt::Display for PullError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::SourceMissing(s) => write!(f, "source missing: {s}"),
            Self::HttpUnavailable => {
                write!(f, "HTTP download requires --features model-pull-http")
            }
            Self::HttpError(s) => write!(f, "download error: {s}"),
        }
    }
}

impl std::error::Error for PullError {}

impl From<io::Error> for PullError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Pull a model from `source` into the tilth model cache under `name`.
///
/// `source` is either a local directory path (starts with `/`, `./`, or `..`)
/// or an HTTP(S) URL. For local sources the files are copied; for remote
/// sources they are downloaded (requires the `model-pull-http` feature).
///
/// Returns the destination directory on success.
pub fn run_model_pull(name: &str, source: &str) -> Result<PathBuf, PullError> {
    let dest = ModelConfig::from_name(name).model_path;
    let dest_dir = dest.parent().expect("model path always has a parent");
    fs::create_dir_all(dest_dir)?;

    if is_url(source) {
        pull_http(source, dest_dir)
    } else {
        pull_local(Path::new(source), dest_dir)
    }?;

    Ok(dest_dir.to_path_buf())
}

fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

fn pull_local(src_dir: &Path, dest_dir: &Path) -> Result<(), PullError> {
    for file in ["model.onnx", "tokenizer.json"] {
        let src = src_dir.join(file);
        if !src.exists() {
            return Err(PullError::SourceMissing(src.display().to_string()));
        }
        let dst = dest_dir.join(file);
        fs::copy(&src, &dst)?;
    }
    Ok(())
}

#[allow(unused_variables)]
fn pull_http(url_prefix: &str, dest_dir: &Path) -> Result<(), PullError> {
    // HTTP download is opt-in via a feature flag to keep the default build
    // free of network-stack dependencies.
    #[cfg(feature = "model-pull-http")]
    {
        for file in ["model.onnx", "tokenizer.json"] {
            let url = format!("{}/{}", url_prefix.trim_end_matches('/'), file);
            let dst = dest_dir.join(file);
            download_file(&url, &dst)?;
        }
        Ok(())
    }
    #[cfg(not(feature = "model-pull-http"))]
    {
        Err(PullError::HttpUnavailable)
    }
}

#[cfg(feature = "model-pull-http")]
fn download_file(url: &str, dst: &Path) -> Result<(), PullError> {
    use std::io::Write as _;
    let response = ureq::get(url)
        .call()
        .map_err(|e| PullError::HttpError(e.to_string()))?;
    let mut reader = response.into_reader();
    let mut file = fs::File::create(dst)?;
    io::copy(&mut reader, &mut file)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn pull_local_copies_files() {
        let src = tempdir().unwrap();
        let dst = tempdir().unwrap();

        // Create fake model files in the source dir.
        fs::write(src.path().join("model.onnx"), b"fake-onnx").unwrap();
        fs::write(src.path().join("tokenizer.json"), b"{}").unwrap();

        pull_local(src.path(), dst.path()).expect("pull_local must succeed");

        assert!(dst.path().join("model.onnx").exists());
        assert!(dst.path().join("tokenizer.json").exists());
        assert_eq!(
            fs::read(dst.path().join("model.onnx")).unwrap(),
            b"fake-onnx"
        );
    }

    #[test]
    fn pull_local_errors_on_missing_source() {
        let dst = tempdir().unwrap();
        let src = tempdir().unwrap();
        // Only write one of the two required files.
        fs::write(src.path().join("model.onnx"), b"fake").unwrap();

        let err = pull_local(src.path(), dst.path()).unwrap_err();
        assert!(
            matches!(err, PullError::SourceMissing(_)),
            "expected SourceMissing, got: {err}"
        );
    }

    #[test]
    fn pull_error_display_io() {
        let e = PullError::Io(io::Error::new(io::ErrorKind::NotFound, "no such file"));
        assert!(e.to_string().contains("I/O error"), "{e}");
    }

    #[test]
    fn pull_error_display_http_unavailable() {
        let e = PullError::HttpUnavailable;
        assert!(e.to_string().contains("HTTP"), "{e}");
    }
}
