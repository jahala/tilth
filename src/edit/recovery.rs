//! Recover from a stale tag by replaying the parsed ops against a cached
//! snapshot and 3-way-merging onto live content. Ported from oh-my-pi
//! `packages/hashline/src/recovery.ts`.
//!
//! Strategy order:
//! 1. Replay ops on the cached snapshot, diff snapshot→result, and apply that
//!    delta onto the live content with EXACT context matching (diffy's
//!    `apply` shifts position but never fuzzes context — the fuzz-0 equivalent;
//!    it returns `Err` cleanly on no-match, never panics).
//! 2. Session-chain fallback: when the snapshot was not the head, replay ops
//!    directly onto live iff line counts match AND every anchor line is
//!    byte-identical between snapshot and live.
//! 3. Otherwise reject with [`MismatchError`].

#![allow(dead_code)]

use std::path::Path;

use thiserror::Error;

use super::apply::{anchor_lines, apply_ops, lower_ops, ApplyError, ApplyResult};
use super::mismatch::MismatchError;
use super::parser::Op;
use super::snapshots::{Snapshot, SnapshotStore};
use super::tag::compute_file_hash;

/// Attempt recovery for a stale-tag incident. Returns the recovered text or a
/// [`MismatchError`] describing why recovery failed.
///
/// # Errors
///
/// Returns [`MismatchError::Fabricated`] when `tag` was never recorded this
/// session, or [`MismatchError::Drift`] when the ops cannot be replayed onto
/// the live text.
pub fn try_recover(
    store: &SnapshotStore,
    path: &Path,
    tag: u16,
    ops: &[Op],
    live: &str,
) -> Result<String, MismatchError> {
    // Derive the store key through the crate's single canonical-key owner so a
    // tag recorded under a canonical realpath is found here regardless of the
    // raw path spelling (e.g. macOS case divergence).
    let key = super::normalize_path_key(path);
    let Some(snapshot) = store.by_tag(&key, tag) else {
        return Err(if store.find_by_tag(tag).is_empty() {
            MismatchError::Fabricated {
                path: key,
                expected_tag: tag,
            }
        } else {
            MismatchError::Drift {
                path: key,
                expected_tag: tag,
                actual_tag: compute_file_hash(live),
            }
        });
    };

    let is_head = store.head_tag(&key) == Some(tag);

    // Strategy 1: replay on snapshot, 3-way-merge the delta onto live.
    if let Some(merged) = merge_onto_live(path, &snapshot.text, live, ops) {
        return Ok(merged);
    }

    // Strategy 2: session-chain replay onto live directly.
    if !is_head {
        if let Some(text) = replay_session_chain(path, &snapshot, live, ops) {
            return Ok(text);
        }
    }

    // Both strategies discard their ApplyError, so a `replace_text` whose `old`
    // no longer resolves against live would surface as bare drift and send the
    // agent re-reading a file whose text simply does not contain the anchor.
    // Re-lower against live purely to recover that diagnosis — but only when a
    // text swap is present, since lowering a block op re-parses the outline
    // uncached (~86ms on 735KB of Rust) for a diagnosis it cannot produce.
    if ops.iter().any(|o| matches!(o, Op::TextSwap { .. })) {
        if let Err(err) = lower_ops(path, live, ops) {
            if err.is_text_match_failure() {
                return Err(MismatchError::TextMatch {
                    path: key,
                    source: err,
                });
            }
        }
    }

    Err(MismatchError::Drift {
        path: key,
        expected_tag: tag,
        actual_tag: compute_file_hash(live),
    })
}

fn merge_onto_live(path: &Path, snapshot: &str, live: &str, ops: &[Op]) -> Option<String> {
    let applied = apply_ops(path, snapshot, ops).ok()?;
    if applied.text == snapshot {
        return None;
    }
    let patch = diffy::create_patch(snapshot, &applied.text);
    // diffy::apply matches context exactly (fuzz-0) and returns Err on no-match.
    let merged = diffy::apply(live, &patch).ok()?;
    if merged == live {
        return None;
    }
    Some(merged)
}

fn replay_session_chain(
    path: &Path,
    snapshot: &Snapshot,
    live: &str,
    ops: &[Op],
) -> Option<String> {
    let prev: Vec<&str> = snapshot.text.split('\n').collect();
    let curr: Vec<&str> = live.split('\n').collect();
    if prev.len() != curr.len() {
        return None;
    }
    let (line_ops, _) = lower_ops(path, live, ops).ok()?;
    let anchors = anchor_lines(&line_ops);
    // These anchors resolved against LIVE, so check_seen_lines never saw them.
    if snapshot
        .first_unseen_anchor(anchors.iter().copied())
        .is_some()
    {
        return None;
    }
    for a in anchors {
        let idx = (a as usize).checked_sub(1)?;
        if idx >= prev.len() || idx >= curr.len() || prev[idx] != curr[idx] {
            return None;
        }
    }
    let applied = apply_ops(path, live, ops).ok()?;
    if applied.text == live {
        return None;
    }
    Some(applied.text)
}

/// seenLines gate for the no-drift path: reject an edit anchored on a line the
/// producer never displayed under this tag. A snapshot with no recorded
/// provenance (empty `seen_lines`) skips the check.
pub fn check_seen_lines(snapshot: &Snapshot, path: &Path, ops: &[Op]) -> Result<(), MismatchError> {
    // Lowering failures (unresolved block anchor, file-op conflict, bad range)
    // skip the gate: apply_ops re-lowers this same text and reports the real
    // ApplyError. Live-lowering paths re-check provenance themselves — see
    // replay_session_chain.
    let Ok((line_ops, _)) = lower_ops(path, &snapshot.text, ops) else {
        return Ok(());
    };
    match snapshot.first_unseen_anchor(anchor_lines(&line_ops)) {
        Some(line) => Err(MismatchError::UnseenAnchor {
            path: snapshot.path.clone(),
            line,
        }),
        None => Ok(()),
    }
}

/// Failure from the composed edit egress: either the provenance gate rejected
/// the edit, or applying the ops to the gated snapshot failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EditError {
    #[error(transparent)]
    Mismatch(#[from] MismatchError),
    #[error(transparent)]
    Apply(#[from] ApplyError),
}

/// The composed no-drift edit egress: enforce the seenLines provenance gate on
/// `snapshot`, then apply `ops` to the snapshot text. This is the single
/// entrypoint PR2 wires; raw `apply_ops` stays module-internal so no caller can
/// apply edits while skipping the gate.
///
/// # Errors
///
/// Returns [`EditError::Mismatch`] when the provenance gate rejects the edit,
/// or [`EditError::Apply`] when applying the ops fails.
pub fn gated_apply(snapshot: &Snapshot, path: &Path, ops: &[Op]) -> Result<ApplyResult, EditError> {
    check_seen_lines(snapshot, path, ops)?;
    Ok(apply_ops(path, &snapshot.text, ops)?)
}

#[cfg(test)]
mod tests {
    use super::super::parser::Cursor;
    use super::*;
    use std::path::PathBuf;

    fn p() -> PathBuf {
        PathBuf::from("recovery_fixture.rs")
    }

    fn swap(line: u32, to: &str) -> Vec<Op> {
        vec![Op::Swap {
            start: line,
            end: line,
            payload: vec![to.to_string()],
        }]
    }

    #[test]
    fn moved_but_unchanged_block_recovers_via_three_way_merge() {
        let mut store = SnapshotStore::new();
        let snapshot = "line1\nline2\nTARGET\nline4\nline5\n";
        let key = p().to_string_lossy().into_owned();
        let tag = store.record(&key, snapshot, []).unwrap();

        // External edit prepended a line, shifting TARGET from line 3 to line 4.
        let live = "PREPENDED\nline1\nline2\nTARGET\nline4\nline5\n";
        let recovered = try_recover(&store, &p(), tag, &swap(3, "CHANGED"), live)
            .expect("moved block recovers");
        assert_eq!(
            recovered,
            "PREPENDED\nline1\nline2\nCHANGED\nline4\nline5\n"
        );
    }

    #[test]
    fn conflicting_edit_yields_drift() {
        let mut store = SnapshotStore::new();
        let snapshot = "a\nb\nTARGET\nd\n";
        let key = p().to_string_lossy().into_owned();
        let tag = store.record(&key, snapshot, []).unwrap();

        // Live diverged everywhere — the patch context cannot match.
        let live = "totally\ndifferent\ncontent\nhere\n";
        let err = try_recover(&store, &p(), tag, &swap(3, "NEW"), live).unwrap_err();
        assert!(
            matches!(err, MismatchError::Drift { expected_tag, .. } if expected_tag == tag),
            "{err:?}"
        );
    }

    /// A drifted file whose live text no longer contains the `replace_text`
    /// anchor must name the match failure. Reporting bare drift tells the agent
    /// to re-read, which cannot fix text that simply is not there.
    #[test]
    fn drifted_text_swap_reports_the_match_failure_not_bare_drift() {
        let mut store = SnapshotStore::new();
        let snapshot = "a\nlet x = OLDNAME;\nc\n";
        let key = p().to_string_lossy().into_owned();
        let tag = store.record(&key, snapshot, []).unwrap();

        // External edit removed the anchor text entirely and changed the shape.
        let live = "a\nlet x = SOMETHING_ELSE;\nc\nextra\n";
        let ops = vec![Op::TextSwap {
            old: "OLDNAME".to_string(),
            new: "NEWNAME".to_string(),
        }];
        let err = try_recover(&store, &p(), tag, &ops, live).unwrap_err();
        // Branch on the variant, not its prose — the point of carrying the
        // ApplyError is that callers can tell the match failures apart.
        let MismatchError::TextMatch { source, .. } = &err else {
            panic!("expected TextMatch, got {err:?}");
        };
        assert!(
            matches!(source, ApplyError::TextUnmatched { .. }),
            "expected TextUnmatched, got {source:?}"
        );
    }

    /// The specific-error path must not swallow genuine drift.
    #[test]
    fn drifted_text_swap_that_still_matches_yields_drift() {
        let mut store = SnapshotStore::new();
        let snapshot = "a\nlet x = OLDNAME;\nc\n";
        let key = p().to_string_lossy().into_owned();
        let tag = store.record(&key, snapshot, []).unwrap();

        // Anchor still present, but live diverged so recovery declines.
        let live = "totally\ndifferent\nlet x = OLDNAME;\nand\nmore\n";
        let ops = vec![Op::TextSwap {
            old: "OLDNAME".to_string(),
            new: "NEWNAME".to_string(),
        }];
        // Must reject — accepting an Ok here would let a wrong recovery pass,
        // which is the regression this test exists to catch.
        let err = try_recover(&store, &p(), tag, &ops, live)
            .expect_err("divergent live must not recover");
        assert!(
            matches!(err, MismatchError::Drift { .. }),
            "a resolvable anchor must not report TextMatch, got {err:?}"
        );
    }

    /// Positive counterpart to `ambiguous_in_snapshot_unique_in_live_must_not_edit_an_unseen_line`:
    /// a session-chain recovery whose anchor line WAS displayed under this tag
    /// (non-empty `seen_lines` covering it) must still succeed. An off-by-one
    /// in the provenance guard would silently kill strategy-2 recovery for
    /// every read that records provenance, and every other test here uses an
    /// empty seen set, so nothing else would catch it.
    #[test]
    fn session_chain_recovery_succeeds_when_anchor_line_was_seen() {
        let mut store = SnapshotStore::new();
        let snapshot = "line1\nline2\nTARGET\nline4\nline5\n";
        let key = p().to_string_lossy().into_owned();
        let tag = store.record(&key, snapshot, [1u32, 2, 3]).unwrap();

        // Record a later snapshot so `tag` is no longer the head, forcing
        // `try_recover` to consider the session-chain strategy at all.
        let _ = store.record(&key, "line1\nline2\nTARGET\nline4\nline5\nline6\n", []);

        // Live diverged on line2 (not the anchor), so strategy 1's exact-context
        // patch cannot match and must fall through to the session-chain replay.
        // Line count matches the snapshot, and the anchored line (TARGET) is
        // byte-identical, so the session-chain strategy can land the edit.
        let live = "line1\nCHANGED2\nTARGET\nline4\nline5\n";
        let ops = vec![Op::TextSwap {
            old: "TARGET".to_string(),
            new: "RECOVERED".to_string(),
        }];
        let recovered =
            try_recover(&store, &p(), tag, &ops, live).expect("seen anchor must recover");
        assert_eq!(recovered, "line1\nCHANGED2\nRECOVERED\nline4\nline5\n");
    }

    /// Probe: `check_seen_lines` skips the provenance gate whenever `lower_ops`
    /// fails, and `replace_text` makes that failure content-triggerable. If an
    /// `old` that is ambiguous in the snapshot is unique in live, the gate is
    /// skipped and `replay_session_chain` lowers against live — potentially
    /// landing the edit on a line the read never displayed.
    #[test]
    fn ambiguous_in_snapshot_unique_in_live_must_not_edit_an_unseen_line() {
        let mut store = SnapshotStore::new();
        let mut lines: Vec<String> = (1..=40).map(|i| format!("line{i}")).collect();
        lines[1] = "ANCHOR".to_string(); // line 2 — displayed
        lines[39] = "ANCHOR".to_string(); // line 40 — never displayed
        let snapshot = lines.join("\n") + "\n";
        let key = p().to_string_lossy().into_owned();
        // Only lines 1-3 were ever shown to the model.
        let tag = store.record(&key, &snapshot, [1u32, 2, 3]).unwrap();
        // A later snapshot makes `tag` non-head so strategy 2 is reachable.
        let mut newer = lines.clone();
        newer[10] = "unrelated".to_string();
        let _ = store.record(&key, &(newer.join("\n") + "\n"), []);

        // External edit removed the line-2 occurrence, preserving line count.
        let mut live_lines = lines.clone();
        live_lines[1] = "SOMETHING_ELSE".to_string();
        let live = live_lines.join("\n") + "\n";

        let ops = vec![Op::TextSwap {
            old: "ANCHOR".to_string(),
            new: "PWNED".to_string(),
        }];

        // Compose exactly as the write egress does: gate, then recover.
        let snap = store.by_tag(&key, tag).expect("snapshot recorded");

        // The gate still skips here — lowering against the snapshot is
        // ambiguous, and on the no-drift path apply_ops re-lowers the same text
        // and reports that accurately. So the skip itself is not the bug.
        assert!(
            check_seen_lines(&snap, &p(), &ops).is_ok(),
            "gate is expected to skip an unlowerable anchor; the bypass is downstream"
        );

        // Recovery must refuse: strategy 2 resolves against live, so line 40 is
        // reachable there even though it was never displayed under this tag.
        match try_recover(&store, &p(), tag, &ops, &live) {
            Ok(text) => {
                panic!("provenance bypass: edit landed on a line the read never displayed:\n{text}")
            }
            Err(e) => assert!(
                matches!(
                    e,
                    MismatchError::TextMatch { .. } | MismatchError::Drift { .. }
                ),
                "unexpected rejection: {e:?}"
            ),
        }
    }

    #[test]
    fn unknown_tag_yields_fabricated() {
        let store = SnapshotStore::new();
        let bogus_tag = 0xBEEF;
        let err = try_recover(&store, &p(), bogus_tag, &swap(1, "x"), "a\nb\n").unwrap_err();
        assert!(
            matches!(err, MismatchError::Fabricated { expected_tag, .. } if expected_tag == bogus_tag),
            "{err:?}"
        );
    }

    #[test]
    fn session_chain_recovers_when_not_head() {
        let mut store = SnapshotStore::new();
        let key = p().to_string_lossy().into_owned();
        // v1 (stale tag the model will anchor against).
        let v1 = "a\nb\nTARGET\nd\n";
        let tag1 = store.record(&key, v1, []).unwrap();
        // v2 is the head — an in-session edit changed line 2 (b → MODIFIED).
        let v2 = "a\nMODIFIED\nTARGET\nd\n";
        store.record(&key, v2, []).unwrap();

        // Model edits against the stale tag1; live == v2. Strategy 1's patch
        // context (which includes the old line 2 "b") cannot match live, so the
        // session-chain fallback applies the edit directly onto live.
        let live = v2;
        let recovered =
            try_recover(&store, &p(), tag1, &swap(3, "NEW"), live).expect("session chain recovers");
        assert_eq!(recovered, "a\nMODIFIED\nNEW\nd\n");
    }

    #[test]
    fn seen_lines_gate_rejects_undisplayed_anchor() {
        let mut store = SnapshotStore::new();
        let key = p().to_string_lossy().into_owned();
        // Only lines 1-3 were displayed under this tag.
        let tag = store
            .record(&key, "l1\nl2\nl3\nl4\nl5\n", [1, 2, 3])
            .unwrap();
        let snap = store.by_tag(&key, tag).unwrap();

        // An edit anchored on line 5 (never displayed) is rejected.
        let err = check_seen_lines(&snap, &p(), &swap(5, "x")).unwrap_err();
        assert!(
            matches!(err, MismatchError::UnseenAnchor { line: 5, .. }),
            "{err:?}"
        );

        // An edit anchored on a displayed line passes.
        assert!(check_seen_lines(&snap, &p(), &swap(2, "x")).is_ok());
    }

    #[test]
    fn seen_lines_gate_covers_insert_anchor() {
        let mut store = SnapshotStore::new();
        let key = p().to_string_lossy().into_owned();
        let tag = store.record(&key, "l1\nl2\nl3\n", [1, 2]).unwrap();
        let snap = store.by_tag(&key, tag).unwrap();
        let ins = vec![Op::Ins {
            cursor: Cursor::Pre(3),
            payload: vec!["x".into()],
        }];
        let err = check_seen_lines(&snap, &p(), &ins).unwrap_err();
        assert!(
            matches!(err, MismatchError::UnseenAnchor { line: 3, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn empty_seen_lines_skips_gate() {
        let mut store = SnapshotStore::new();
        let key = p().to_string_lossy().into_owned();
        let tag = store.record(&key, "l1\nl2\n", []).unwrap();
        let snap = store.by_tag(&key, tag).unwrap();
        // No provenance recorded → gate is skipped.
        assert!(check_seen_lines(&snap, &p(), &swap(2, "x")).is_ok());
    }

    #[test]
    fn recovery_key_is_canonical_not_raw_spelling() {
        let mut store = SnapshotStore::new();
        // Record under the canonical key, exactly as the recording path does.
        let canonical = super::super::normalize_path_key(&p());
        let snapshot = "line1\nTARGET\nline3\n";
        let tag = store.record(&canonical, snapshot, []).unwrap();

        // Recover using a differently-spelled path that canonicalizes to the
        // same key. A raw `to_string_lossy` key ("./recovery_fixture.rs") would
        // miss the recorded canonical key and fail recovery.
        let raw_spelling = PathBuf::from("./recovery_fixture.rs");
        assert_ne!(
            raw_spelling.to_string_lossy(),
            canonical,
            "raw spelling must differ from the canonical key for this test to bite"
        );
        let recovered = try_recover(&store, &raw_spelling, tag, &swap(2, "CHANGED"), snapshot)
            .expect("canonical key lookup recovers despite raw path spelling");
        assert_eq!(recovered, "line1\nCHANGED\nline3\n");
    }

    #[test]
    fn gated_apply_enforces_seen_lines_then_applies() {
        let snap = Snapshot {
            path: "g.rs".into(),
            text: "l1\nl2\nl3\n".into(),
            tag: 0,
            recorded_at: 1,
            seen_lines: [1, 2].into_iter().collect(),
        };

        // An edit on an unseen line is rejected by the composed gate.
        let err = gated_apply(&snap, &p(), &swap(3, "x")).unwrap_err();
        assert_eq!(
            err,
            EditError::Mismatch(MismatchError::UnseenAnchor {
                path: "g.rs".into(),
                line: 3,
            })
        );

        // An edit on a seen line passes the gate and applies to snapshot text.
        let result = gated_apply(&snap, &p(), &swap(2, "CHANGED")).unwrap();
        assert_eq!(result.text, "l1\nCHANGED\nl3\n");
    }

    #[test]
    fn gate_skips_on_lowering_failure_so_apply_surfaces_real_error() {
        let snap = Snapshot {
            path: "g.rs".into(),
            text: "l1\nl2\n".into(),
            tag: 0,
            recorded_at: 1,
            seen_lines: [1].into_iter().collect(),
        };
        // REM combined with another op fails lowering (FileOpConflict). The
        // gate must not misreport that as UnseenAnchor { line: 0 }.
        let mut ops = vec![Op::Rem];
        ops.extend(swap(1, "x"));

        assert!(check_seen_lines(&snap, &p(), &ops).is_ok());
        let err = gated_apply(&snap, &p(), &ops).unwrap_err();
        assert_eq!(err, EditError::Apply(ApplyError::FileOpConflict));
    }
}
