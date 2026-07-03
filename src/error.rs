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
    #[error("{} [denied by .tilthignore]", path.display())]
    IgnoreDenied { path: PathBuf },
    #[error("invalid query \"{query}\": {reason}")]
    InvalidQuery { query: String, reason: String },
    #[error("{}: {source}", path.display())]
    IoError {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parse error in {}: {reason}", path.display())]
    ParseError { path: PathBuf, reason: String },
    /// A whole-file-tag edit was rejected: the section's tag no longer matches
    /// live content and recovery declined (Drift), the tag was never minted this
    /// session (Fabricated), or the edit anchored a line the read never
    /// displayed (`UnseenAnchor`). Carries the already-actionable mismatch
    /// message verbatim.
    #[error("{0}")]
    EditRejected(String),
}

impl From<crate::edit::mismatch::MismatchError> for TilthError {
    fn from(e: crate::edit::mismatch::MismatchError) -> Self {
        TilthError::EditRejected(e.to_string())
    }
}

impl From<crate::edit::recovery::EditError> for TilthError {
    fn from(e: crate::edit::recovery::EditError) -> Self {
        use crate::edit::recovery::EditError;
        match e {
            EditError::Mismatch(m) => m.into(),
            EditError::Apply(a) => TilthError::EditRejected(a.to_string()),
        }
    }
}

impl TilthError {
    /// Exit code matching the spec.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NotFound { .. } | Self::IoError { .. } => 2,
            Self::InvalidQuery { .. } | Self::ParseError { .. } | Self::EditRejected(_) => 3,
            Self::PermissionDenied { .. } | Self::IgnoreDenied { .. } => 4,
        }
    }
}
