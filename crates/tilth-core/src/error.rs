use std::path::PathBuf;

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
    IoError {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parse error in {}: {reason}", path.display())]
    ParseError { path: PathBuf, reason: String },
    #[error("{} changed on disk while the edit was being applied — re-read the file and retry with the new hashes; nothing was written", path.display())]
    ConcurrentModification { path: PathBuf },
}

impl TilthError {
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
