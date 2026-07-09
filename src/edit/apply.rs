//! Apply parsed [`Op`]s to file text.
//!
//! Ported from oh-my-pi `packages/hashline/src/apply.ts`, but the ~1300 lines of
//! that file are line-by-line boundary-repair heuristics that exist only because
//! the reference model decomposes every edit into single-line inserts+deletes
//! and must then reconcile indentation/brace drift. tilth's [`Op`] model carries
//! whole ranges and cursors, so a splice is exact — none of the boundary-repair
//! machinery (and none of its JSX-specific pieces) is needed. What is kept:
//! overlap rejection, bounds checking, and deferred-block lowering.

#![allow(dead_code)]

use std::borrow::Cow;
use std::path::Path;

use super::block::{outline_for, resolve_block_in};
use super::parser::{BlockMode, Cursor, Op};

/// File-level operation surfaced separately from the text edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    Remove,
    Move(String),
}

impl FileOp {
    /// Extract the file-level op (`REM`/`MV`) from `ops`, enforcing the same
    /// conflict guard [`lower_ops`] applies: at most one file op per section,
    /// and `REM` cannot combine with content edits. Returns `Ok(None)` for a
    /// pure content edit. The canonical mapping every apply/recovery path shares
    /// so no branch hand-derives a file op that bypasses these guards.
    ///
    /// # Errors
    ///
    /// Returns [`ApplyError::FileOpConflict`] when two file ops appear, or `REM`
    /// combines with a content op.
    pub fn from_ops(ops: &[Op]) -> Result<Option<FileOp>, ApplyError> {
        let mut file_op: Option<FileOp> = None;
        let mut has_content = false;
        for op in ops {
            match op {
                Op::Rem => {
                    if file_op.is_some() {
                        return Err(ApplyError::FileOpConflict);
                    }
                    file_op = Some(FileOp::Remove);
                }
                Op::Mv { dest } => {
                    if file_op.is_some() {
                        return Err(ApplyError::FileOpConflict);
                    }
                    file_op = Some(FileOp::Move(dest.clone()));
                }
                _ => has_content = true,
            }
        }
        // REM deletes the whole file; it cannot combine with content edits.
        if matches!(file_op, Some(FileOp::Remove)) && has_content {
            return Err(ApplyError::FileOpConflict);
        }
        Ok(file_op)
    }
}

/// Result of applying ops to a text body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyResult {
    /// Post-edit text.
    pub text: String,
    /// First 1-based line that changed, or `None` for a no-op.
    pub first_changed_line: Option<usize>,
    /// A file-level op (`REM`/`MV`), if present.
    pub file_op: Option<FileOp>,
}

/// Why an apply failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApplyError {
    /// Two ranged ops touch overlapping lines.
    #[error("overlapping edit ranges: {}.={} and {}.={}", a.0, a.1, b.0, b.1)]
    Overlap { a: (u32, u32), b: (u32, u32) },
    /// A line anchor is outside `1..=total`.
    #[error("line {line} out of bounds (file has {total} lines)")]
    OutOfBounds { line: u32, total: usize },
    /// A block anchor could not be resolved to a span.
    #[error("could not resolve block anchor: {0}")]
    BlockUnresolved(String),
    /// `REM` combined with other ops, or more than one file op.
    #[error("REM cannot combine with other ops; only one file op per section")]
    FileOpConflict,
}

/// A lowered, concrete line op — no blocks, no file ops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineOp {
    Swap {
        start: u32,
        end: u32,
        payload: Vec<String>,
    },
    Del {
        start: u32,
        end: u32,
    },
    Ins {
        cursor: Cursor,
        payload: Vec<String>,
    },
}

/// Lower `ops` into concrete [`LineOp`]s by resolving block anchors against
/// `text` (language inferred from `path`). File ops are returned separately.
pub(super) fn lower_ops(
    path: &Path,
    text: &str,
    ops: &[Op],
) -> Result<(Vec<LineOp>, Option<FileOp>), ApplyError> {
    // File ops (REM/MV) plus the one-file-op / REM-alone conflict guard live in
    // the canonical `FileOp::from_ops`; the loop below handles only content ops.
    let file_op = FileOp::from_ops(ops)?;
    let mut line_ops = Vec::new();

    // Parse the outline once when any block ops need span resolution: each
    // `get_outline_entries` call re-parses the whole file, so resolving K block
    // anchors must not trigger K parses.
    let outline = if ops.iter().any(|o| matches!(o, Op::Block { .. })) {
        outline_for(path, text)
    } else {
        None
    };
    for op in ops {
        match op {
            Op::Swap {
                start,
                end,
                payload,
            } => {
                line_ops.push(LineOp::Swap {
                    start: *start,
                    end: *end,
                    payload: payload.clone(),
                });
            }
            Op::Del { start, end } => {
                line_ops.push(LineOp::Del {
                    start: *start,
                    end: *end,
                });
            }
            Op::Ins { cursor, payload } => {
                line_ops.push(LineOp::Ins {
                    cursor: cursor.clone(),
                    payload: payload.clone(),
                });
            }
            Op::Block {
                anchor,
                mode,
                payload,
            } => {
                let span = outline
                    .as_deref()
                    .and_then(|entries| resolve_block_in(entries, anchor))
                    .ok_or_else(|| ApplyError::BlockUnresolved(format!("{anchor:?}")))?;
                match mode {
                    BlockMode::Swap => line_ops.push(LineOp::Swap {
                        start: span.start,
                        end: span.end,
                        payload: payload.clone(),
                    }),
                    BlockMode::Del => line_ops.push(LineOp::Del {
                        start: span.start,
                        end: span.end,
                    }),
                    BlockMode::InsPost => line_ops.push(LineOp::Ins {
                        cursor: Cursor::Post(span.end),
                        payload: payload.clone(),
                    }),
                }
            }
            Op::Rem | Op::Mv { .. } => {}
        }
    }

    Ok((line_ops, file_op))
}

/// The anchor lines an op set reads, for recovery's session-chain content check.
pub(super) fn anchor_lines(ops: &[LineOp]) -> Vec<u32> {
    let mut lines = Vec::new();
    for op in ops {
        match op {
            LineOp::Swap { start, end, .. } | LineOp::Del { start, end } => {
                for l in *start..=*end {
                    lines.push(l);
                }
            }
            LineOp::Ins { cursor, .. } => match cursor {
                Cursor::Pre(n) | Cursor::Post(n) => lines.push(*n),
                Cursor::Head | Cursor::Tail => {}
            },
        }
    }
    lines
}

/// Apply `ops` to `text`, resolving block anchors against `path`.
///
/// # Errors
///
/// Returns [`ApplyError`] when ops overlap, an anchor is out of bounds or
/// unresolved, or file ops conflict.
pub(super) fn apply_ops(path: &Path, text: &str, ops: &[Op]) -> Result<ApplyResult, ApplyError> {
    let (line_ops, file_op) = lower_ops(path, text, ops)?;
    let mut result = apply_line_ops(text, &line_ops)?;
    result.file_op = file_op;
    Ok(result)
}

/// One resolved splice against the original line vector.
struct Splice {
    idx: usize, // 0-based start index in the split vector
    len: usize, // number of original elements consumed
    new: Vec<String>,
    // Inclusive range this splice occupies for overlap checking; `None` for
    // pure inserts (zero-width points).
    range: Option<(u32, u32)>,
}

/// Splice `line_ops` into `text`, treating it as `split('\n')` rows with 1-based
/// line numbers (self-consistent with the whole-file-tag numbered-line render).
pub(super) fn apply_line_ops(text: &str, line_ops: &[LineOp]) -> Result<ApplyResult, ApplyError> {
    let rows: Vec<Cow<str>> = text.split('\n').map(Cow::Borrowed).collect();
    let total = rows.len();

    let mut splices: Vec<Splice> = Vec::with_capacity(line_ops.len());
    for op in line_ops {
        match op {
            LineOp::Swap {
                start,
                end,
                payload,
            } => {
                check_bounds(*start, total)?;
                check_bounds(*end, total)?;
                splices.push(Splice {
                    idx: (*start - 1) as usize,
                    len: (*end - *start + 1) as usize,
                    new: payload.clone(),
                    range: Some((*start, *end)),
                });
            }
            LineOp::Del { start, end } => {
                check_bounds(*start, total)?;
                check_bounds(*end, total)?;
                splices.push(Splice {
                    idx: (*start - 1) as usize,
                    len: (*end - *start + 1) as usize,
                    new: Vec::new(),
                    range: Some((*start, *end)),
                });
            }
            LineOp::Ins { cursor, payload } => {
                let idx = match cursor {
                    Cursor::Pre(n) => {
                        check_bounds(*n, total)?;
                        (*n - 1) as usize
                    }
                    Cursor::Post(n) => {
                        check_bounds(*n, total)?;
                        *n as usize
                    }
                    Cursor::Head => 0,
                    Cursor::Tail => {
                        if text.ends_with('\n') {
                            total - 1
                        } else {
                            total
                        }
                    }
                };
                splices.push(Splice {
                    idx,
                    len: 0,
                    new: payload.clone(),
                    range: None,
                });
            }
        }
    }

    reject_overlaps(&splices)?;

    let first_changed_line = splices.iter().map(|s| s.idx + 1).min();

    // Apply in descending index order so earlier splices don't invalidate the
    // indices of later ones.
    let mut order: Vec<usize> = (0..splices.len()).collect();
    order.sort_by(|&a, &b| {
        splices[b].idx.cmp(&splices[a].idx).then_with(|| {
            // At the same index, apply the ranged splice before a zero-width
            // insert so the insert lands relative to the post-splice rows rather
            // than mid-range (e.g. `INS.PRE 2` + `SWAP 2.=3`, order-independent).
            let rank = |s: &Splice| usize::from(s.range.is_none());
            rank(&splices[a]).cmp(&rank(&splices[b])).then_with(|| {
                // Among equal-idx zero-width inserts, apply the later
                // original op first so sequential same-idx splices land in
                // author order (each later splice pushes earlier ones back).
                if splices[a].range.is_none() && splices[b].range.is_none() {
                    b.cmp(&a)
                } else {
                    std::cmp::Ordering::Equal
                }
            })
        })
    });

    let mut owned = rows;
    for &i in &order {
        let s = &splices[i];
        let end = (s.idx + s.len).min(owned.len());
        owned.splice(s.idx..end, s.new.iter().map(|l| Cow::Owned(l.clone())));
    }

    let out = owned.join("\n");
    let first_changed_line = if out == text {
        None
    } else {
        first_changed_line
    };
    Ok(ApplyResult {
        text: out,
        first_changed_line,
        file_op: None,
    })
}

fn check_bounds(line: u32, total: usize) -> Result<(), ApplyError> {
    if line < 1 || line as usize > total {
        return Err(ApplyError::OutOfBounds { line, total });
    }
    Ok(())
}

/// Reject any two ranged splices that overlap, and any insert landing inside a
/// ranged splice.
fn reject_overlaps(splices: &[Splice]) -> Result<(), ApplyError> {
    let ranged: Vec<(usize, (u32, u32))> = splices
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.range.map(|r| (i, r)))
        .collect();

    // Ranged vs ranged.
    for i in 0..ranged.len() {
        for j in (i + 1)..ranged.len() {
            let (_, a) = ranged[i];
            let (_, b) = ranged[j];
            if a.0 <= b.1 && b.0 <= a.1 {
                return Err(ApplyError::Overlap { a, b });
            }
        }
    }

    // Insert landing inside a ranged op.
    for s in splices {
        if s.range.is_some() {
            continue;
        }
        // Insert idx is 0-based; the 1-based "anchor line" it targets is idx or
        // idx+1 depending on Pre/Post, but either way it must not fall strictly
        // inside a ranged [start,end].
        for (_, r) in &ranged {
            let anchor = s.idx as u32; // Pre(n)→n-1, Post(n)→n, Head→0, Tail→total
            if anchor + 1 > r.0 && anchor < r.1 {
                return Err(ApplyError::Overlap {
                    a: *r,
                    b: (anchor, anchor),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p() -> PathBuf {
        PathBuf::from("apply_fixture.rs")
    }

    fn apply(text: &str, ops: &[Op]) -> Result<ApplyResult, ApplyError> {
        apply_ops(&p(), text, ops)
    }

    #[test]
    fn swap_replaces_range() {
        let r = apply(
            "a\nb\nc\nd\n",
            &[Op::Swap {
                start: 2,
                end: 3,
                payload: vec!["X".into(), "Y".into()],
            }],
        )
        .unwrap();
        assert_eq!(r.text, "a\nX\nY\nd\n");
        assert_eq!(r.first_changed_line, Some(2));
    }

    #[test]
    fn del_removes_range() {
        let r = apply("a\nb\nc\nd\n", &[Op::Del { start: 2, end: 3 }]).unwrap();
        assert_eq!(r.text, "a\nd\n");
    }

    #[test]
    fn ins_pre_and_post() {
        let r = apply(
            "a\nb\n",
            &[Op::Ins {
                cursor: Cursor::Pre(2),
                payload: vec!["mid".into()],
            }],
        )
        .unwrap();
        assert_eq!(r.text, "a\nmid\nb\n");

        let r = apply(
            "a\nb\n",
            &[Op::Ins {
                cursor: Cursor::Post(1),
                payload: vec!["mid".into()],
            }],
        )
        .unwrap();
        assert_eq!(r.text, "a\nmid\nb\n");
    }

    #[test]
    fn ins_head_and_tail() {
        let r = apply(
            "a\nb",
            &[Op::Ins {
                cursor: Cursor::Head,
                payload: vec!["top".into()],
            }],
        )
        .unwrap();
        assert_eq!(r.text, "top\na\nb");

        let r = apply(
            "a\nb",
            &[Op::Ins {
                cursor: Cursor::Tail,
                payload: vec!["bot".into()],
            }],
        )
        .unwrap();
        assert_eq!(r.text, "a\nb\nbot");
    }

    #[test]
    fn multiple_ops_apply_independently() {
        let r = apply(
            "a\nb\nc\nd\ne\n",
            &[
                Op::Swap {
                    start: 1,
                    end: 1,
                    payload: vec!["A".into()],
                },
                Op::Del { start: 4, end: 4 },
            ],
        )
        .unwrap();
        assert_eq!(r.text, "A\nb\nc\ne\n");
    }

    #[test]
    fn block_swap_by_symbol_resolves_and_replaces() {
        let src = "fn alpha() {\n    1\n}\n\nfn beta() {\n    2\n}\n";
        let r = apply(
            src,
            &[Op::Block {
                anchor: super::super::parser::BlockAnchor::Symbol("beta".into()),
                mode: BlockMode::Swap,
                payload: vec!["fn beta() { 99 }".into()],
            }],
        )
        .unwrap();
        assert_eq!(r.text, "fn alpha() {\n    1\n}\n\nfn beta() { 99 }\n");
    }

    #[test]
    fn block_del_by_line_removes_span() {
        let src = "fn alpha() {\n    1\n}\n\nfn beta() {\n    2\n}\n";
        let r = apply(
            src,
            &[Op::Block {
                anchor: super::super::parser::BlockAnchor::Line(1),
                mode: BlockMode::Del,
                payload: vec![],
            }],
        )
        .unwrap();
        assert_eq!(r.text, "\nfn beta() {\n    2\n}\n");
    }

    #[test]
    fn overlapping_ranges_rejected() {
        let err = apply(
            "a\nb\nc\nd\n",
            &[
                Op::Swap {
                    start: 1,
                    end: 3,
                    payload: vec!["X".into()],
                },
                Op::Del { start: 2, end: 2 },
            ],
        )
        .unwrap_err();
        assert_eq!(
            err,
            ApplyError::Overlap {
                a: (1, 3),
                b: (2, 2)
            }
        );
    }

    #[test]
    fn out_of_bounds_rejected() {
        let err = apply("a\nb\n", &[Op::Del { start: 9, end: 9 }]).unwrap_err();
        assert_eq!(err, ApplyError::OutOfBounds { line: 9, total: 3 });
    }

    #[test]
    fn rem_reports_file_op() {
        let r = apply("a\nb\n", &[Op::Rem]).unwrap();
        assert_eq!(r.file_op, Some(FileOp::Remove));
        assert_eq!(r.text, "a\nb\n", "REM does not edit text");
    }

    #[test]
    fn mv_reports_file_op() {
        let r = apply(
            "a\n",
            &[Op::Mv {
                dest: "b.rs".into(),
            }],
        )
        .unwrap();
        assert_eq!(r.file_op, Some(FileOp::Move("b.rs".into())));
    }

    #[test]
    fn rem_with_content_op_conflicts() {
        let err = apply("a\nb\n", &[Op::Rem, Op::Del { start: 1, end: 1 }]).unwrap_err();
        assert_eq!(err, ApplyError::FileOpConflict);
    }

    #[test]
    fn mv_leaves_text_untouched() {
        let r = apply(
            "a\nb\n",
            &[Op::Mv {
                dest: "b.rs".into(),
            }],
        )
        .unwrap();
        assert_eq!(r.text, "a\nb\n", "MV is a file op — text is unchanged");
        assert_eq!(r.first_changed_line, None);
    }

    #[test]
    fn ins_head_into_empty_file() {
        let r = apply(
            "",
            &[Op::Ins {
                cursor: Cursor::Head,
                payload: vec!["x".into()],
            }],
        )
        .unwrap();
        assert_eq!(r.text, "x\n");
        assert_eq!(r.first_changed_line, Some(1));
    }

    #[test]
    fn single_line_file_swap_and_delete() {
        // A file with no trailing newline is one row.
        let swapped = apply(
            "abc",
            &[Op::Swap {
                start: 1,
                end: 1,
                payload: vec!["X".into()],
            }],
        )
        .unwrap();
        assert_eq!(swapped.text, "X");
        assert_eq!(swapped.first_changed_line, Some(1));

        let deleted = apply("abc", &[Op::Del { start: 1, end: 1 }]).unwrap();
        assert_eq!(deleted.text, "");
        assert_eq!(deleted.first_changed_line, Some(1));
    }

    #[test]
    fn swap_first_and_last_content_line() {
        // "a\nb\nc\n" splits to [a, b, c, ""]; line 3 is the last content line.
        let r = apply(
            "a\nb\nc\n",
            &[
                Op::Swap {
                    start: 1,
                    end: 1,
                    payload: vec!["A".into()],
                },
                Op::Swap {
                    start: 3,
                    end: 3,
                    payload: vec!["C".into()],
                },
            ],
        )
        .unwrap();
        assert_eq!(r.text, "A\nb\nC\n");
        assert_eq!(r.first_changed_line, Some(1));
    }

    #[test]
    fn adjacent_ranges_are_allowed() {
        // (1,2) and (3,4) touch but do not overlap — both must apply.
        let r = apply(
            "a\nb\nc\nd\ne\n",
            &[
                Op::Swap {
                    start: 1,
                    end: 2,
                    payload: vec!["X".into()],
                },
                Op::Swap {
                    start: 3,
                    end: 4,
                    payload: vec!["Y".into()],
                },
            ],
        )
        .unwrap();
        assert_eq!(r.text, "X\nY\ne\n");
    }

    #[test]
    fn touching_shared_line_ranges_are_rejected() {
        // (2,3) and (3,4) share line 3 → genuine overlap.
        let err = apply(
            "a\nb\nc\nd\ne\n",
            &[
                Op::Swap {
                    start: 2,
                    end: 3,
                    payload: vec!["X".into()],
                },
                Op::Swap {
                    start: 3,
                    end: 4,
                    payload: vec!["Y".into()],
                },
            ],
        )
        .unwrap_err();
        assert!(matches!(err, ApplyError::Overlap { .. }), "{err:?}");
    }

    #[test]
    fn crlf_line_endings_survive_untouched_rows() {
        // apply operates on raw text; a swap rewrites its own row while other
        // rows keep their CRLF verbatim.
        let r = apply(
            "a\r\nb\r\nc\r\n",
            &[Op::Swap {
                start: 2,
                end: 2,
                payload: vec!["B".into()],
            }],
        )
        .unwrap();
        assert_eq!(r.text, "a\r\nB\nc\r\n");
    }

    #[test]
    fn insert_at_range_top_boundary_is_order_independent() {
        // `INS.PRE 2` (insert before line 2) + `SWAP 2.=3` share 0-based idx 1.
        // The result must not depend on op-list order: the ranged swap applies
        // first, then the insert lands before the swapped rows.
        let ins = Op::Ins {
            cursor: Cursor::Pre(2),
            payload: vec!["mid".into()],
        };
        let swap = Op::Swap {
            start: 2,
            end: 3,
            payload: vec!["X".into()],
        };

        let insert_first = apply("a\nb\nc\nd\n", &[ins.clone(), swap.clone()]).unwrap();
        assert_eq!(insert_first.text, "a\nmid\nX\nd\n");

        let swap_first = apply("a\nb\nc\nd\n", &[swap, ins]).unwrap();
        assert_eq!(swap_first.text, "a\nmid\nX\nd\n");

        assert_eq!(insert_first.text, swap_first.text);
    }

    #[test]
    fn ins_tail_on_newline_terminated_file_appends_as_new_line() {
        // "a\nb\n" splits into ["a","b",""] — Tail must resolve to the phantom
        // empty row's index so append lands as new content, not after it.
        let r = apply(
            "a\nb\n",
            &[Op::Ins {
                cursor: Cursor::Tail,
                payload: vec!["x".into()],
            }],
        )
        .unwrap();
        assert_eq!(r.text, "a\nb\nx\n");
    }

    #[test]
    fn multiple_appends_land_in_author_order() {
        let r = apply(
            "a\n",
            &[
                Op::Ins {
                    cursor: Cursor::Tail,
                    payload: vec!["x".into()],
                },
                Op::Ins {
                    cursor: Cursor::Tail,
                    payload: vec!["y".into()],
                },
            ],
        )
        .unwrap();
        assert_eq!(r.text, "a\nx\ny\n");
    }

    #[test]
    fn multiple_insert_befores_at_same_line_land_in_author_order() {
        let r = apply(
            "a\nb\n",
            &[
                Op::Ins {
                    cursor: Cursor::Pre(2),
                    payload: vec!["x".into()],
                },
                Op::Ins {
                    cursor: Cursor::Pre(2),
                    payload: vec!["y".into()],
                },
            ],
        )
        .unwrap();
        assert_eq!(r.text, "a\nx\ny\nb\n");
    }
}
