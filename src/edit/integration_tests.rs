//! End-to-end integration across the whole-file-tag subsystem. Drives the real
//! `parse_sections → apply_ops → SnapshotStore → try_recover` pipeline against
//! the public APIs with no stage stubbed, asserting on final text (not Ok/Err).
//! The per-module unit tests cover each stage in isolation; these lock the
//! seams between them.

use std::path::Path;

use super::apply::apply_ops;
use super::mismatch::MismatchError;
use super::parser::parse_sections;
use super::recovery::try_recover;
use super::snapshots::SnapshotStore;
use super::tag::{compute_file_hash, format_tag};

/// Read mints a tag; the model echoes `[path#TAG]` and a multi-op section; the
/// tag still matches live content, so the parsed ops apply directly.
#[test]
fn parse_apply_pipeline_multi_op_on_unchanged_content() {
    let path = Path::new("pipeline_fixture.rs");
    let key = path.to_string_lossy().into_owned();
    let orig = "a\nb\nc\nd\n";

    let mut store = SnapshotStore::new();
    let tag = store.record(path, orig, [1, 2, 3, 4]).unwrap();

    let blob = format!(
        "[{key}#{}]\nSWAP 1:\n+A\nINS.POST 2:\n+X\nDEL 4\n",
        format_tag(tag)
    );
    let mut sections = parse_sections(&blob).expect("parse");
    assert_eq!(sections.len(), 1);
    let section = sections.pop().unwrap();
    assert_eq!(section.path, key);
    assert_eq!(
        section.tag,
        Some(tag),
        "parsed tag round-trips the minted tag"
    );

    // Tag matches live → no recovery needed; apply the parsed ops directly.
    assert_eq!(
        compute_file_hash(orig),
        tag,
        "unchanged content still hashes to the tag"
    );
    let applied = apply_ops(path, orig, &section.ops).expect("apply");
    assert_eq!(applied.text, "A\nb\nX\nc\n");
    assert_eq!(applied.first_changed_line, Some(1));
    assert!(applied.file_op.is_none());
}

/// After an external prepend, live no longer hashes to the tag. The full
/// pipeline recovers the moved-but-unchanged target via 3-way merge and lands
/// the edit at the shifted position.
#[test]
fn parse_apply_recover_pipeline_after_external_drift() {
    let path = Path::new("pipeline_drift.rs");
    let key = path.to_string_lossy().into_owned();
    let orig = "alpha\nbeta\nTARGET\ndelta\nepsilon\n";

    let mut store = SnapshotStore::new();
    let tag = store.record(path, orig, [1, 2, 3, 4, 5]).unwrap();

    let blob = format!("[{key}#{}]\nSWAP 3:\n+RECOVERED\n", format_tag(tag));
    let section = parse_sections(&blob).expect("parse").pop().unwrap();
    assert_eq!(section.tag, Some(tag));

    // External edit prepended two lines: TARGET moves from line 3 to line 5.
    let live = "NEW1\nNEW2\nalpha\nbeta\nTARGET\ndelta\nepsilon\n";
    assert_ne!(
        compute_file_hash(live),
        tag,
        "external drift invalidates the tag"
    );

    let recovered = try_recover(&store, path, tag, &section.ops, live)
        .expect("moved-but-unchanged target recovers via 3-way merge");
    assert_eq!(
        recovered,
        "NEW1\nNEW2\nalpha\nbeta\nRECOVERED\ndelta\nepsilon\n"
    );
}

/// A tag never recorded in this session drives the pipeline to a `Fabricated`
/// rejection — the parse succeeds, but recovery has no snapshot to replay from.
#[test]
fn parse_recover_pipeline_rejects_fabricated_tag() {
    let path = Path::new("pipeline_fabricated.rs");
    let key = path.to_string_lossy().into_owned();
    let store = SnapshotStore::new(); // empty — no read ever minted a tag

    let blob = format!("[{key}#BEEF]\nSWAP 1:\n+x\n");
    let section = parse_sections(&blob).expect("parse").pop().unwrap();
    let tag = section.tag.expect("tag parsed");

    let err = try_recover(&store, path, tag, &section.ops, "a\nb\n").unwrap_err();
    assert!(
        matches!(err, MismatchError::Fabricated { expected_tag, .. } if expected_tag == tag),
        "{err:?}"
    );
}
