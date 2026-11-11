use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Every error tilth can produce. Displayed as user-facing messages with suggestions.
#[derive(Debug, Error)]
pub enum TilthError {
    #[error("not found: {}{}", path.display(), suggestion.as_deref().map_or(String::new(), |s| format!(" — did you mean: {s}")))]
    NotFound {
        path: PathBuf,
        suggestion: Option<String>,
    },
    #[error("{} [permission denied]", path.display())]
    PermissionDenied { path: PathBuf },
    #[error("{} already exists — pass `overwrite: true` to replace it", path.display())]
    AlreadyExists { path: PathBuf },
    #[error("invalid query \"{query}\": {reason}")]
    InvalidQuery { query: String, reason: String },
    #[error("{}: {source}", path.display())]
    IoError { path: PathBuf, source: io::Error },
    #[error("parse error in {}: {reason}", path.display())]
    ParseError { path: PathBuf, reason: String },
    #[error("{} changed on disk while the edit was being applied — re-read the file and retry with the new hashes; nothing was written", path.display())]
    ConcurrentModification { path: PathBuf },
}

impl TilthError {
    /// The error for an I/O failure on `path`: the one mapping from an I/O
    /// error's kind to what tilth reports. `suggest` names a similar path,
    /// and is called only when `path` is missing.
    #[must_use]
    pub fn from_io(
        path: &Path,
        source: io::Error,
        suggest: impl FnOnce() -> Option<String>,
    ) -> Self {
        let path = path.to_path_buf();
        match source.kind() {
            io::ErrorKind::NotFound => Self::NotFound {
                path,
                suggestion: suggest(),
            },
            io::ErrorKind::PermissionDenied => Self::PermissionDenied { path },
            _ => Self::IoError { path, source },
        }
    }

    /// Exit code matching the spec.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NotFound { .. }
            | Self::IoError { .. }
            | Self::AlreadyExists { .. }
            | Self::ConcurrentModification { .. } => 2,
            Self::InvalidQuery { .. } | Self::ParseError { .. } => 3,
            Self::PermissionDenied { .. } => 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing path is `NotFound` with the suggestion, asked for only
    /// then; a refused one is `PermissionDenied`; any other failure keeps
    /// its I/O error.
    #[test]
    fn an_io_error_maps_by_its_kind() {
        let path = Path::new("src/mian.rs");
        let suggest = || Some("src/main.rs".to_string());
        let error = |kind| io::Error::from(kind);

        let missing = TilthError::from_io(path, error(io::ErrorKind::NotFound), suggest);
        assert_eq!(
            missing.to_string(),
            "not found: src/mian.rs — did you mean: src/main.rs"
        );
        let missing = TilthError::from_io(path, error(io::ErrorKind::NotFound), || None);
        assert_eq!(missing.to_string(), "not found: src/mian.rs");

        let unasked = || -> Option<String> { panic!("a suggestion for a path that exists") };
        let denied = TilthError::from_io(path, error(io::ErrorKind::PermissionDenied), unasked);
        assert_eq!(denied.to_string(), "src/mian.rs [permission denied]");
        assert_eq!(denied.exit_code(), 4);
        let other = TilthError::from_io(path, error(io::ErrorKind::InvalidData), unasked);
        assert_eq!(other.to_string(), "src/mian.rs: invalid data");
        assert_eq!(other.exit_code(), 2);
    }
}
