//! Rejection error when a section tag doesn't match live content and recovery
//! failed. Ported from oh-my-pi `packages/hashline/src/mismatch.ts`: two shapes,
//! `Drift` (tag was minted this session but content moved on) vs `Fabricated`
//! (tag never seen — hallucinated or carried over from a prior session).
//!
//! PR2 maps this onto a new `TilthError` variant; PR1 keeps it standalone.

#![allow(dead_code)]

/// A tag/content mismatch that recovery could not resolve.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MismatchError {
    /// The expected tag was recorded this session, but the live file hashes to
    /// something else and recovery declined the merge.
    #[error(
        "Edit rejected for {path}: file changed between read and edit. \
             Section is bound to #{expected_tag:04X}, but the current file hashes to \
             #{actual_tag:04X}. Re-read to refresh the tag before retrying."
    )]
    Drift {
        path: String,
        expected_tag: u16,
        actual_tag: u16,
    },
    /// The expected tag was never recorded — likely a hallucinated tag or one
    /// reused from a prior session.
    #[error(
        "Edit rejected for {path}: tag #{expected_tag:04X} is not from this session. \
             Re-read the file to copy a current [path#tag] header — never invent a tag."
    )]
    Fabricated { path: String, expected_tag: u16 },
    /// An edit anchored on a line the read never displayed under this tag.
    #[error(
        "Edit rejected for {path}: line {line} was never displayed under this tag. \
             Re-read the region you intend to edit."
    )]
    UnseenAnchor { path: String, line: u32 },
    /// A `replace_text` anchor did not resolve against the live file. The
    /// specific match failure is what the caller must act on — reporting it as
    /// generic drift sends the agent into a re-read loop that cannot help.
    /// Carries the [`ApplyError`] itself so callers can branch on which kind of
    /// match failure it was, rather than parsing its rendered text.
    #[error(
        "Edit rejected for {path}: {source}. The file also changed since the read \
             that minted this tag — re-read to refresh it."
    )]
    TextMatch {
        path: String,
        #[source]
        source: super::apply::ApplyError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drift_message_names_both_tags() {
        let e = MismatchError::Drift {
            path: "src/a.rs".into(),
            expected_tag: 0x1A2B,
            actual_tag: 0x3C4D,
        };
        let s = e.to_string();
        assert!(s.contains("#1A2B"), "{s}");
        assert!(s.contains("#3C4D"), "{s}");
        assert!(s.contains("changed between read and edit"), "{s}");
    }

    #[test]
    fn fabricated_message_flags_unknown_tag() {
        let e = MismatchError::Fabricated {
            path: "src/a.rs".into(),
            expected_tag: 0x9F3E,
        };
        let s = e.to_string();
        assert!(s.contains("not from this session"), "{s}");
        assert!(s.contains("#9F3E"), "{s}");
    }
}
