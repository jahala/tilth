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
    Create(String),
    Remove,
    Move(String),
}

impl FileOp {
    /// Extract the file-level op (`CREATE`/`REM`/`MV`) from `ops`, enforcing
    /// at most one file op per section. `CREATE` and `REM` cannot combine with
    /// content edits; `MV` may carry content edits for the destination.
    pub fn from_ops(ops: &[Op]) -> Result<Option<FileOp>, ApplyError> {
        let mut file_op: Option<FileOp> = None;
        let mut has_content = false;
        for op in ops {
            match op {
                Op::Create { content } => {
                    if file_op.is_some() {
                        return Err(ApplyError::FileOpConflict);
                    }
                    file_op = Some(FileOp::Create(content.clone()));
                }
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
        // CREATE and REM operate on the whole file; neither combines with content edits.
        if matches!(file_op, Some(FileOp::Create(_) | FileOp::Remove)) && has_content {
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
    /// A file-level op (`CREATE`/`REM`/`MV`), if present.
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
    /// A file op (`CREATE`/`REM`/`MV`) combined with content ops, or more than one file op.
    #[error(
        "CREATE/REM cannot combine with content ops; at most one file op (CREATE/REM/MV) per section"
    )]
    FileOpConflict,
    /// The requested text did not occur in the source.
    #[error("text to replace was not found; re-read the file and retry (preview: {preview})")]
    TextUnmatched { preview: String },
    /// The requested text occurred more than once in the source. `count` is a
    /// lower bound — the search stops once ambiguity is established.
    #[error("text to replace matched at least {count} times; add context so it matches once")]
    TextAmbiguous { count: usize },
    /// `old` was empty; a match-once anchor cannot be empty.
    #[error("replace_text \"old\" must not be empty")]
    TextOldEmpty,
}

impl ApplyError {
    /// Whether this describes a `replace_text` anchor that did not resolve, as
    /// opposed to a structural lowering failure. Lives with the variants so a
    /// new text-anchor error cannot silently fall through a consumer's match.
    pub(crate) fn is_text_match_failure(&self) -> bool {
        matches!(
            self,
            ApplyError::TextUnmatched { .. }
                | ApplyError::TextAmbiguous { .. }
                | ApplyError::TextOldEmpty
        )
    }
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

/// Resolve a unique literal text occurrence's byte span in `text`, counting
/// possibly-overlapping occurrences so a self-overlapping needle (e.g.
/// `old:"---"` in `"----"`) is reported as ambiguous rather than unique.
///
/// Stops at the second occurrence. The guard only needs "more than one", and
/// `old` is caller-supplied and bounded only by the snapshot cap: counting
/// every occurrence costs O(n·m), which measured 187s for a 200KB `old`
/// against a 400KB file — enough to wedge the single-threaded server.
fn find_text_span(text: &str, old: &str) -> Result<(usize, usize), ApplyError> {
    let Some(lead) = old.chars().next() else {
        return Err(ApplyError::TextOldEmpty);
    };
    let Some(start) = text.find(old) else {
        return Err(ApplyError::TextUnmatched {
            preview: old.chars().take(80).collect(),
        });
    };
    // `old` occupies `start`, so `lead`'s width lands on the next char boundary
    // — the earliest a second, possibly overlapping, occurrence could begin.
    let next = start + lead.len_utf8();
    if next <= text.len() && text[next..].contains(old) {
        return Err(ApplyError::TextAmbiguous { count: 2 });
    }
    Ok((start, start + old.len()))
}

fn line_number(text: &str, byte_pos: usize) -> u32 {
    text[..byte_pos].bytes().filter(|&b| b == b'\n').count() as u32 + 1
}

/// Render the covering lines of `edits` with each substitution applied.
///
/// `edits` must be non-empty and sorted ascending by start, with no overlapping
/// byte ranges — [`lower_text_swaps`] is the only producer and establishes all
/// three. The caller already knows the line span, so this returns only the
/// replacement lines rather than rescanning the prefix to recompute it.
fn render_swapped_lines(text: &str, edits: &[(usize, usize, &str)]) -> Vec<String> {
    let first_start = edits[0].0;
    let last_end = edits[edits.len() - 1].1;
    let line_start = text[..first_start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[last_end..]
        .find('\n')
        .map_or(text.len(), |i| last_end + i);
    let mut out = String::new();
    let mut cursor = line_start;
    for &(s, e, new) in edits {
        out.push_str(&text[cursor..s]);
        out.push_str(new);
        cursor = e;
    }
    out.push_str(&text[cursor..line_end]);
    out.split('\n').map(str::to_string).collect()
}

/// Resolve a unique literal text occurrence to a whole-line replacement span.
///
/// Test-only: production lowers text swaps through [`lower_text_swaps`], which
/// also coalesces same-line swaps. This composes the same two primitives
/// ([`find_text_span`] then [`render_swapped_lines`]) for single-swap cases, and
/// is confined to `cfg(test)` so it cannot be mistaken for a second entry point.
#[cfg(test)]
fn resolve_text_swap(
    text: &str,
    old: &str,
    new: &str,
) -> Result<(u32, u32, Vec<String>), ApplyError> {
    let (start, end) = find_text_span(text, old)?;
    Ok((
        line_number(text, start),
        line_number(text, end),
        render_swapped_lines(text, &[(start, end, new)]),
    ))
}

/// Resolve every `Op::TextSwap` in `ops` against `text`, coalescing swaps whose
/// covering line spans overlap into a single [`LineOp::Swap`] so two substring
/// replacements on one line both apply instead of colliding in
/// [`reject_overlaps`]. Two swaps whose matched byte ranges genuinely overlap
/// still error. Returns one slot per op index in `ops`; `None` for
/// non-`TextSwap` ops and for ops merged into an earlier slot.
fn lower_text_swaps(text: &str, ops: &[Op]) -> Result<Vec<Option<LineOp>>, ApplyError> {
    struct Resolved<'a> {
        op_idx: usize,
        start: usize,
        end: usize,
        new: &'a str,
        line_span: (u32, u32),
    }

    let mut resolved: Vec<Resolved> = Vec::new();
    for (op_idx, op) in ops.iter().enumerate() {
        if let Op::TextSwap { old, new } = op {
            let (start, end) = find_text_span(text, old)?;
            resolved.push(Resolved {
                op_idx,
                start,
                end,
                new,
                line_span: (line_number(text, start), line_number(text, end)),
            });
        }
    }
    // Sorting by byte start puts members of a run adjacent and makes the error
    // reported for a conflicting pair deterministic; grouping through a HashMap
    // left it dependent on iteration order.
    resolved.sort_by_key(|r| r.start);

    let mut out: Vec<Option<LineOp>> = vec![None; ops.len()];
    let mut run_start = 0;
    while run_start < resolved.len() {
        // Extend the run while the next swap's covering lines touch it. Keying
        // on an exact (start_line, end_line) match instead would split two
        // swaps that share a start line but differ in end line, and
        // reject_overlaps would then reject the very pair coalescing exists
        // to join.
        let mut run_end = run_start + 1;
        let mut last_line = resolved[run_start].line_span.1;
        while run_end < resolved.len() && resolved[run_end].line_span.0 <= last_line {
            if resolved[run_end].start < resolved[run_end - 1].end {
                return Err(ApplyError::Overlap {
                    a: resolved[run_end - 1].line_span,
                    b: resolved[run_end].line_span,
                });
            }
            last_line = last_line.max(resolved[run_end].line_span.1);
            run_end += 1;
        }

        let run = &resolved[run_start..run_end];
        let edits: Vec<(usize, usize, &str)> =
            run.iter().map(|r| (r.start, r.end, r.new)).collect();
        let payload = render_swapped_lines(text, &edits);
        let primary = run.iter().map(|r| r.op_idx).min().unwrap_or(0);
        out[primary] = Some(LineOp::Swap {
            start: run[0].line_span.0,
            end: last_line,
            payload,
        });
        run_start = run_end;
    }

    Ok(out)
}

/// Lower `ops` into concrete [`LineOp`]s by resolving block anchors against
/// `text` (language inferred from `path`). File ops are returned separately.
pub(super) fn lower_ops(
    path: &Path,
    text: &str,
    ops: &[Op],
) -> Result<(Vec<LineOp>, Option<FileOp>), ApplyError> {
    // File ops (CREATE/REM/MV) plus the one-file-op conflict guard live in
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
    // Text swaps resolve against the pristine `text` up front so overlapping
    // and same-line groups can be detected/coalesced before lowering.
    let mut text_swaps = lower_text_swaps(text, ops)?;
    for (i, op) in ops.iter().enumerate() {
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
            Op::TextSwap { .. } => {
                if let Some(line_op) = text_swaps[i].take() {
                    line_ops.push(line_op);
                }
            }
            Op::Create { .. } | Op::Rem | Op::Mv { .. } => {}
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

    #[test]
    fn resolve_text_swap_unique_match_substitutes() {
        let result = resolve_text_swap("alpha target omega\n", "target", "replacement")
            .expect("unique match");
        assert_eq!(result, (1, 1, vec!["alpha replacement omega".to_string()]));
    }

    #[test]
    fn resolve_text_swap_zero_matches_errors() {
        let err = resolve_text_swap("alpha\n", "target", "replacement").unwrap_err();
        assert_eq!(
            err,
            ApplyError::TextUnmatched {
                preview: "target".into()
            }
        );
    }

    #[test]
    fn resolve_text_swap_multi_match_errors() {
        let err = resolve_text_swap("target\ntarget\n", "target", "replacement").unwrap_err();
        assert_eq!(err, ApplyError::TextAmbiguous { count: 2 });
    }

    #[test]
    fn resolve_text_swap_mid_line_span() {
        let text = "head OLD-one\nmiddle two TAIL\nend\n";
        let result = resolve_text_swap(text, "OLD-one\nmiddle two", "NEW\nreplacement")
            .expect("unique multi-line match");
        assert_eq!(
            result,
            (
                1,
                2,
                vec!["head NEW".to_string(), "replacement TAIL".to_string()]
            )
        );
    }

    #[test]
    fn resolve_text_swap_multibyte_final_char_applies_exactly() {
        let text = "prefix café";
        let result = resolve_text_swap(text, "é", "X").expect("unique match");
        assert_eq!(result, (1, 1, vec!["prefix cafX".to_string()]));
        let applied = apply_line_ops(
            text,
            &[LineOp::Swap {
                start: result.0,
                end: result.1,
                payload: result.2,
            }],
        )
        .expect("lowered op applies");
        assert_eq!(applied.text, "prefix cafX");
    }

    #[test]
    fn resolve_text_swap_newline_only_applies_exactly() {
        let text = "a\nb";
        let result = resolve_text_swap(text, "\n", "X").expect("unique match");
        assert_eq!(result, (1, 2, vec!["aXb".to_string()]));
        let applied = apply_line_ops(
            text,
            &[LineOp::Swap {
                start: result.0,
                end: result.1,
                payload: result.2,
            }],
        )
        .expect("lowered op applies");
        assert_eq!(applied.text, "aXb");
    }

    #[test]
    fn resolve_text_swap_interior_newline_applies_exactly() {
        let text = "a\nb\nc";
        let result = resolve_text_swap(text, "a\nb", "X\nY").expect("unique match");
        assert_eq!(result, (1, 2, vec!["X".to_string(), "Y".to_string()]));
        let applied = apply_line_ops(
            text,
            &[LineOp::Swap {
                start: result.0,
                end: result.1,
                payload: result.2,
            }],
        )
        .expect("lowered op applies");
        assert_eq!(applied.text, "X\nY\nc");
    }

    #[test]
    fn resolve_text_swap_newline_ending_at_eof_applies_exactly() {
        let text = "a\nb\n";
        let result = resolve_text_swap(text, "b\n", "X").expect("unique match");
        assert_eq!(result, (2, 3, vec!["X".to_string()]));
        let applied = apply_line_ops(
            text,
            &[LineOp::Swap {
                start: result.0,
                end: result.1,
                payload: result.2,
            }],
        )
        .expect("lowered op applies");
        assert_eq!(applied.text, "a\nX");
    }

    /// A self-overlapping needle has more occurrences than `match_indices`
    /// reports; counting non-overlapping matches made the uniqueness guard
    /// fail open and silently apply one of several candidate spans.
    #[test]
    fn resolve_text_swap_self_overlapping_match_is_ambiguous() {
        let err = resolve_text_swap("----", "---", "X").expect_err("two true occurrences");
        assert_eq!(err, ApplyError::TextAmbiguous { count: 2 });
    }

    /// Three overlapping occurrences must still be caught. `count` is a lower
    /// bound: the scan stops once ambiguity is established, because counting
    /// them all is O(n·m) in a caller-supplied needle.
    #[test]
    fn resolve_text_swap_self_overlapping_triple_is_ambiguous() {
        let err = resolve_text_swap("/////", "///", "//").expect_err("three true occurrences");
        let ApplyError::TextAmbiguous { count } = err else {
            panic!("expected TextAmbiguous, got {err:?}");
        };
        assert!(
            count >= 2,
            "count is a lower bound on occurrences, got {count}"
        );
    }

    /// The early exit must not scan the whole file: a large self-overlapping
    /// needle used to take minutes here.
    #[test]
    fn resolve_text_swap_large_self_overlapping_needle_returns_promptly() {
        let text = "a".repeat(400_000);
        let old = "a".repeat(200_000);
        let started = std::time::Instant::now();
        let err = resolve_text_swap(&text, &old, "X").expect_err("ambiguous");
        assert!(matches!(err, ApplyError::TextAmbiguous { .. }), "{err:?}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "took {:?}; the occurrence scan is not short-circuiting",
            started.elapsed()
        );
    }

    /// The overlap fix must not make a genuinely unique match ambiguous.
    #[test]
    fn resolve_text_swap_unique_match_still_applies_after_overlap_fix() {
        let result = resolve_text_swap("let a = foo(1);\n", "foo", "baz").expect("unique");
        assert_eq!(result, (1, 1, vec!["let a = baz(1);".to_string()]));
    }

    #[test]
    fn resolve_text_swap_empty_old_is_rejected() {
        assert_eq!(
            resolve_text_swap("fn f() {}\n", "", "X").expect_err("empty old"),
            ApplyError::TextOldEmpty
        );
    }

    /// An empty file previously let an empty `old` through as a silent insert.
    #[test]
    fn resolve_text_swap_empty_old_on_empty_file_is_rejected() {
        assert_eq!(
            resolve_text_swap("", "", "X").expect_err("empty old"),
            ApplyError::TextOldEmpty
        );
    }

    /// Two disjoint substring replacements on one line are the most natural
    /// `replace_text` batch; each lowers to the same covering line span, so
    /// they must coalesce instead of colliding in `reject_overlaps`.
    #[test]
    fn two_disjoint_text_swaps_on_one_line_both_apply() {
        let text = "let a = foo(bar);\n";
        let ops = vec![
            Op::TextSwap {
                old: "foo".into(),
                new: "baz".into(),
            },
            Op::TextSwap {
                old: "bar".into(),
                new: "qux".into(),
            },
        ];
        let (line_ops, _) = lower_ops(Path::new("a.rs"), text, &ops).expect("both lower");
        let applied = apply_line_ops(text, &line_ops).expect("both apply");
        assert_eq!(applied.text, "let a = baz(qux);\n");
    }

    /// Swaps sharing a start line but ending on different lines used to land in
    /// different groups (the key was the exact line span), so they were emitted
    /// as two `LineOp::Swap`s and then rejected by `reject_overlaps` — the exact
    /// collision coalescing exists to prevent.
    #[test]
    fn text_swaps_sharing_a_start_line_but_not_an_end_line_coalesce() {
        let text = "let a = foo(bar,\n    baz);\ntail\n";
        let ops = vec![
            Op::TextSwap {
                old: "foo".into(),
                new: "qux".into(),
            },
            Op::TextSwap {
                old: "bar,\n    baz".into(),
                new: "ONE\n    TWO".into(),
            },
        ];
        let (line_ops, _) = lower_ops(Path::new("a.rs"), text, &ops).expect("both lower");
        let applied = apply_line_ops(text, &line_ops).expect("both apply");
        assert_eq!(applied.text, "let a = qux(ONE\n    TWO);\ntail\n");
    }

    #[test]
    fn text_swaps_with_overlapping_byte_ranges_still_error() {
        let text = "abcdef\n";
        let ops = vec![
            Op::TextSwap {
                old: "abcd".into(),
                new: "X".into(),
            },
            Op::TextSwap {
                old: "cdef".into(),
                new: "Y".into(),
            },
        ];
        let err = lower_ops(Path::new("a.rs"), text, &ops).expect_err("byte ranges overlap");
        assert!(
            matches!(err, ApplyError::Overlap { .. }),
            "expected Overlap, got {err:?}"
        );
    }

    /// `create_file` combined with a content op raises `FileOpConflict`, so the
    /// message must not name REM — the op the caller never sent.
    #[test]
    fn file_op_conflict_message_names_create_not_only_rem() {
        let err = FileOp::from_ops(&[
            Op::Create {
                content: "x\n".into(),
            },
            Op::Del { start: 1, end: 1 },
        ])
        .expect_err("create + content conflicts");
        assert_eq!(err, ApplyError::FileOpConflict);
        assert_eq!(
            err.to_string(),
            "CREATE/REM cannot combine with content ops; at most one file op (CREATE/REM/MV) per section"
        );
    }
}
