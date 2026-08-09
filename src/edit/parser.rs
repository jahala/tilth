//! Parse a hashline op-grammar text blob into [`Section`]s.
//!
//! Ported from oh-my-pi `packages/hashline/src/{tokenizer,parser}.ts`, expressed
//! against tilth's higher-level [`Op`] model (ranges/cursors) rather than the
//! reference's decompose-to-single-line inserts+deletes. The one tilth-native
//! extension is the `#symbol` anchor on `*.BLK` ops ([`BlockAnchor::Symbol`]).
//!
//! Grammar (1-based inclusive line ranges):
//! ```text
//! [path#TAG]
//! SWAP a.=b:              <payload>
//! DEL n | DEL a.=b
//! INS.PRE n: | INS.POST n:      <payload>
//! INS.HEAD: | INS.TAIL:         <payload>
//! SWAP.BLK n: | SWAP.BLK #sym:  <payload>
//! DEL.BLK n | DEL.BLK #sym
//! INS.BLK.POST n: | INS.BLK.POST #sym:  <payload>
//! REM | MV dest
//! ```

#![allow(dead_code)]

use super::tag::parse_tag;

/// Where an insert lands relative to existing content (1-based line numbers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cursor {
    /// Insert before line `n`.
    Pre(u32),
    /// Insert after line `n`.
    Post(u32),
    /// Insert at the beginning of the file.
    Head,
    /// Insert at the end of the file.
    Tail,
}

/// Anchor for a `*.BLK` op: a line number or a tilth-native `#symbol` name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockAnchor {
    Line(u32),
    Symbol(String),
}

/// Which `*.BLK` op produced a block edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockMode {
    /// `SWAP.BLK` — replace the block's span with the payload.
    Swap,
    /// `DEL.BLK` — delete the block's span.
    Del,
    /// `INS.BLK.POST` — insert the payload after the block's last line.
    InsPost,
}

/// A single parsed op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Replace lines `[start, end]` (inclusive) with `payload`.
    Swap {
        start: u32,
        end: u32,
        payload: Vec<String>,
    },
    /// Replace one exact text occurrence, expanding to its covering lines.
    TextSwap { old: String, new: String },
    /// Delete lines `[start, end]` (inclusive).
    Del { start: u32, end: u32 },
    /// Insert `payload` at `cursor`.
    Ins {
        cursor: Cursor,
        payload: Vec<String>,
    },
    /// Deferred block edit — span resolved against file text at apply time.
    Block {
        anchor: BlockAnchor,
        mode: BlockMode,
        payload: Vec<String>,
    },
    /// Create a new file with exact raw content.
    Create { content: String },
    /// Remove the file.
    Rem,
    /// Move/rename the file to `dest`.
    Mv { dest: String },
}

/// One file section: `[path#TAG]` header plus its ops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub path: String,
    pub tag: Option<u16>,
    pub ops: Vec<Op>,
}

/// Structured parse failure — never a panic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {msg}")]
pub struct ParseError {
    /// 1-based line number the error was detected on.
    pub line: usize,
    pub msg: String,
}

/// A parsed section header `[path#TAG]` or `[path]`.
fn parse_file_header(line: &str) -> Option<(String, Option<u16>)> {
    let t = line.trim();
    let inner = t.strip_prefix('[')?.strip_suffix(']')?;
    if inner.is_empty() {
        return None;
    }
    match inner.rsplit_once('#') {
        Some((path, tag_raw)) => {
            let tag = parse_tag(tag_raw)?;
            if path.is_empty() || path.contains('#') {
                return None;
            }
            Some((path.to_string(), Some(tag)))
        }
        // No `#` at all → a tagless header. A `#` present but not a valid
        // trailing tag falls through `parse_tag` returning None above.
        None => Some((inner.to_string(), None)),
    }
}

/// A header that has been recognized but whose payload (if any) is not yet
/// collected.
#[derive(Debug, Clone)]
enum Proto {
    Swap {
        start: u32,
        end: u32,
    },
    Ins {
        cursor: Cursor,
    },
    Block {
        anchor: BlockAnchor,
        mode: BlockMode,
    },
    // Complete ops (no payload):
    Del {
        start: u32,
        end: u32,
    },
    BlockDel {
        anchor: BlockAnchor,
    },
    Rem,
    Mv {
        dest: String,
    },
}

impl Proto {
    fn takes_payload(&self) -> bool {
        matches!(
            self,
            Proto::Swap { .. } | Proto::Ins { .. } | Proto::Block { .. }
        )
    }
}

fn parse_range(s: &str) -> Option<(u32, u32)> {
    let s = s.trim();
    if let Some((a, b)) = s.split_once(".=") {
        Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
    } else {
        let n: u32 = s.parse().ok()?;
        Some((n, n))
    }
}

fn parse_block_anchor(s: &str) -> Option<BlockAnchor> {
    let s = s.trim();
    if let Some(sym) = s.strip_prefix('#') {
        let sym = sym.trim();
        if sym.is_empty() {
            return None;
        }
        return Some(BlockAnchor::Symbol(sym.to_string()));
    }
    s.parse::<u32>().ok().map(BlockAnchor::Line)
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 2 {
        let f = b[0];
        let l = b[b.len() - 1];
        if (f == b'"' || f == b'\'') && f == l {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

/// Parse a `*.BLK` op body: strip the trailing payload colon, then resolve the
/// line/`#symbol` anchor. `keyword` names the op for error messages.
fn block_payload_anchor(rest: &str, keyword: &str, mode: BlockMode) -> Result<Proto, String> {
    let body = rest
        .trim()
        .strip_suffix(':')
        .ok_or_else(|| format!("{keyword} needs a trailing colon before the payload"))?;
    let anchor = parse_block_anchor(body).ok_or_else(|| {
        format!("{keyword} anchor must be a line number or #symbol, got {body:?}")
    })?;
    Ok(Proto::Block { anchor, mode })
}

/// Parse an `INS.PRE`/`INS.POST` op body: strip the trailing colon, parse the
/// line number, and build the cursor via `mk`. `keyword` names the op.
fn ins_cursor(rest: &str, keyword: &str, mk: impl Fn(u32) -> Cursor) -> Result<Proto, String> {
    let body = rest
        .trim()
        .strip_suffix(':')
        .ok_or_else(|| format!("{keyword} needs a trailing colon"))?;
    let n: u32 = body
        .trim()
        .parse()
        .map_err(|_| format!("{keyword} anchor must be a line number, got {body:?}"))?;
    Ok(Proto::Ins { cursor: mk(n) })
}
/// Try to parse `line` as an op header. `Ok(None)` = not a header;
/// `Ok(Some)` = header; `Err` = a header-shaped line that is malformed.
fn parse_op_header(line: &str) -> Result<Option<Proto>, String> {
    let t = line.trim();
    if t.is_empty() {
        return Ok(None);
    }

    // File-level ops first (exact / prefix, no numeric anchor).
    if t == "REM" {
        return Ok(Some(Proto::Rem));
    }
    if let Some(rest) = t.strip_prefix("MV ") {
        let dest = unquote(rest);
        if dest.is_empty() {
            return Err("MV requires a destination path".into());
        }
        return Ok(Some(Proto::Mv { dest }));
    }

    // Block ops — check the more specific keywords before their prefixes.
    if let Some(rest) = t.strip_prefix("INS.BLK.POST") {
        return Ok(Some(block_payload_anchor(
            rest,
            "INS.BLK.POST",
            BlockMode::InsPost,
        )?));
    }
    if let Some(rest) = t.strip_prefix("SWAP.BLK") {
        return Ok(Some(block_payload_anchor(
            rest,
            "SWAP.BLK",
            BlockMode::Swap,
        )?));
    }
    if let Some(rest) = t.strip_prefix("DEL.BLK") {
        let body = rest.trim();
        if body.ends_with(':') {
            return Err("DEL.BLK takes no colon and no body".into());
        }
        let anchor = parse_block_anchor(body).ok_or_else(|| {
            format!("DEL.BLK anchor must be a line number or #symbol, got {body:?}")
        })?;
        return Ok(Some(Proto::BlockDel { anchor }));
    }

    // Insert ops.
    if t == "INS.HEAD:" {
        return Ok(Some(Proto::Ins {
            cursor: Cursor::Head,
        }));
    }
    if t == "INS.TAIL:" {
        return Ok(Some(Proto::Ins {
            cursor: Cursor::Tail,
        }));
    }
    if let Some(rest) = t.strip_prefix("INS.PRE ") {
        return Ok(Some(ins_cursor(rest, "INS.PRE", Cursor::Pre)?));
    }
    if let Some(rest) = t.strip_prefix("INS.POST ") {
        return Ok(Some(ins_cursor(rest, "INS.POST", Cursor::Post)?));
    }

    // Concrete range ops.
    if let Some(rest) = t.strip_prefix("SWAP ") {
        let body = rest
            .trim()
            .strip_suffix(':')
            .ok_or_else(|| "SWAP needs a trailing colon before the payload".to_string())?;
        let (start, end) = parse_range(body)
            .ok_or_else(|| format!("SWAP range must be `a.=b` or `n`, got {body:?}"))?;
        if end < start {
            return Err(format!("SWAP range {start}.={end} ends before it starts"));
        }
        return Ok(Some(Proto::Swap { start, end }));
    }
    if let Some(rest) = t.strip_prefix("DEL ") {
        let body = rest.trim();
        if body.ends_with(':') {
            return Err("DEL takes no colon and no body".into());
        }
        let (start, end) = parse_range(body)
            .ok_or_else(|| format!("DEL range must be `a.=b` or `n`, got {body:?}"))?;
        if end < start {
            return Err(format!("DEL range {start}.={end} ends before it starts"));
        }
        return Ok(Some(Proto::Del { start, end }));
    }

    Ok(None)
}

/// Strip a single `N:` read-gutter prefix from every *bare* (non-`+`-authored)
/// non-empty row, but only when they all carry one — a mixed set means the `N:`
/// is genuine content. A body whose every stripped remainder is a lone
/// quoted/numeric literal is a numeric-keyed dict/YAML mapping, not read-output
/// paste, and is left untouched. Rows authored with an explicit `+` are never
/// bare and are never touched (mirrors oh-my-pi `parser.ts:329-355`).
fn strip_uniform_gutter(rows: &mut [String], bare: &[bool]) {
    let mut saw_bare = false;
    let mut all_literal_values = true;
    for (row, &is_bare) in rows.iter().zip(bare) {
        if !is_bare || row.trim().is_empty() {
            continue;
        }
        saw_bare = true;
        let Some(n) = gutter_prefix_len(row) else {
            return; // a bare row without a prefix → mixed → strip nothing
        };
        all_literal_values &= is_bare_literal_value(&row[n..]);
    }
    if !saw_bare || all_literal_values {
        return;
    }
    for (row, &is_bare) in rows.iter_mut().zip(bare) {
        if !is_bare || row.trim().is_empty() {
            continue;
        }
        if let Some(n) = gutter_prefix_len(row) {
            *row = row[n..].to_string();
        }
    }
}

/// Whether `s` is a lone quoted or numeric literal (optionally comma-
/// terminated) — the shape of a numeric-keyed dict value. Ports
/// oh-my-pi `BARE_LITERAL_VALUE_RE`.
fn is_bare_literal_value(s: &str) -> bool {
    let core = s.trim();
    let core = core.strip_suffix(',').map_or(core, str::trim_end);
    if core.is_empty() {
        return false;
    }
    let bytes = core.as_bytes();
    if (bytes[0] == b'"' || bytes[0] == b'\'') && core.len() >= 2 {
        let q = bytes[0];
        return bytes[core.len() - 1] == q && !core.as_bytes()[1..core.len() - 1].contains(&q);
    }
    let num = core.strip_prefix(['-', '+']).unwrap_or(core);
    let (int_part, frac_part) = match num.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (num, None),
    };
    if int_part.is_empty() || !int_part.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    match frac_part {
        Some(f) => !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()),
        None => true,
    }
}

/// Length of a leading `<optional ws><digits>:` gutter, or `None`.
fn gutter_prefix_len(row: &str) -> Option<usize> {
    let bytes = row.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    let digit_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == digit_start {
        return None;
    }
    if i < bytes.len() && bytes[i] == b':' {
        return Some(i + 1);
    }
    None
}

/// Normalize a collected payload: strip a single leading `+` sigil per row,
/// strip a uniform read gutter from bare rows only, then drop trailing blanks.
/// A row authored with an explicit `+` is not bare and is never gutter-stripped.
fn finalize_payload(rows: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(rows.len());
    let mut bare: Vec<bool> = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(stripped) = row.strip_prefix('+') {
            out.push(stripped.to_string());
            bare.push(false);
        } else {
            out.push(row);
            bare.push(true);
        }
    }
    strip_uniform_gutter(&mut out, &bare);
    while out.last().is_some_and(|r| r.trim().is_empty()) {
        out.pop();
    }
    out
}

struct Pending {
    proto: Proto,
    line: usize,
    payload: Vec<String>,
}

/// Parse a full op-grammar blob into sections.
///
/// Bare payload rows that look like a `[path#TAG]` header or an op keyword
/// (`REM`, `DEL n`, `MV dest`, …) are read as directives, not payload — author
/// such rows with a `+` prefix to keep them literal.
///
/// # Errors
///
/// Returns [`ParseError`] naming the offending line when the blob does not
/// conform to the grammar.
pub fn parse_sections(input: &str) -> Result<Vec<Section>, ParseError> {
    let mut sections: Vec<Section> = Vec::new();
    let mut current: Option<Section> = None;
    let mut pending: Option<Pending> = None;

    // Flush the pending payload op into the current section.
    macro_rules! flush_pending {
        () => {
            if let Some(p) = pending.take() {
                let Some(sec) = current.as_mut() else {
                    unreachable!("pending op without an open section");
                };
                let payload = finalize_payload(p.payload);
                let op = build_payload_op(p.proto, payload, p.line)?;
                sec.ops.push(op);
            }
        };
    }

    for (idx, raw_line) in input.lines().enumerate() {
        let lineno = idx + 1;

        // File header opens a new section.
        if let Some((path, tag)) = parse_file_header(raw_line) {
            flush_pending!();
            if let Some(sec) = current.take() {
                sections.push(sec);
            }
            current = Some(Section {
                path,
                tag,
                ops: Vec::new(),
            });
            continue;
        }

        // Op header?
        match parse_op_header(raw_line) {
            Err(msg) => return Err(ParseError { line: lineno, msg }),
            Ok(Some(proto)) => {
                flush_pending!();
                if current.is_none() {
                    return Err(ParseError {
                        line: lineno,
                        msg: "op has no preceding [path#TAG] header".into(),
                    });
                }
                if proto.takes_payload() {
                    pending = Some(Pending {
                        proto,
                        line: lineno,
                        payload: Vec::new(),
                    });
                } else {
                    let sec = current.as_mut().unwrap();
                    sec.ops.push(build_complete_op(proto));
                }
                continue;
            }
            Ok(None) => {}
        }

        // Not a header. Either payload for a pending op, a skippable comment,
        // or a stray line.
        if let Some(p) = pending.as_mut() {
            p.payload.push(raw_line.to_string());
            continue;
        }
        if raw_line.trim().is_empty() {
            continue;
        }
        if raw_line.trim_start().starts_with('#') {
            // Skippable comment between ops.
            continue;
        }
        return Err(ParseError {
            line: lineno,
            msg: format!(
                "payload line has no preceding op header: {:?}",
                raw_line.trim_end()
            ),
        });
    }

    flush_pending!();
    if let Some(sec) = current.take() {
        sections.push(sec);
    }
    Ok(sections)
}

fn build_complete_op(proto: Proto) -> Op {
    match proto {
        Proto::Del { start, end } => Op::Del { start, end },
        Proto::BlockDel { anchor } => Op::Block {
            anchor,
            mode: BlockMode::Del,
            payload: Vec::new(),
        },
        Proto::Rem => Op::Rem,
        Proto::Mv { dest } => Op::Mv { dest },
        _ => unreachable!("build_complete_op called on a payload proto"),
    }
}

fn build_payload_op(proto: Proto, payload: Vec<String>, line: usize) -> Result<Op, ParseError> {
    match proto {
        Proto::Swap { start, end } => {
            if payload.is_empty() {
                // A SWAP with an empty body is a pure range deletion.
                return Ok(Op::Del { start, end });
            }
            Ok(Op::Swap {
                start,
                end,
                payload,
            })
        }
        Proto::Ins { cursor } => {
            if payload.is_empty() {
                return Err(ParseError {
                    line,
                    msg: "insert op has an empty payload".into(),
                });
            }
            Ok(Op::Ins { cursor, payload })
        }
        Proto::Block { anchor, mode } => {
            if payload.is_empty() {
                return Err(ParseError {
                    line,
                    msg: "block op has an empty payload".into(),
                });
            }
            Ok(Op::Block {
                anchor,
                mode,
                payload,
            })
        }
        _ => unreachable!("build_payload_op called on a complete proto"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn single(input: &str) -> Section {
        let mut secs = parse_sections(input).expect("parse ok");
        assert_eq!(secs.len(), 1, "expected one section");
        secs.pop().unwrap()
    }

    #[test]
    fn header_with_tag() {
        let sec = single("[src/foo.rs#1A2B]\nDEL 3\n");
        assert_eq!(sec.path, "src/foo.rs");
        assert_eq!(sec.tag, Some(0x1A2B));
    }

    #[test]
    fn header_without_tag() {
        let sec = single("[new.rs]\nINS.HEAD:\n+x\n");
        assert_eq!(sec.path, "new.rs");
        assert_eq!(sec.tag, None);
    }

    #[test]
    fn swap_range_and_payload() {
        let sec = single("[a#0000]\nSWAP 2.=3:\n+line2\n+line3\n");
        assert_eq!(
            sec.ops,
            vec![Op::Swap {
                start: 2,
                end: 3,
                payload: vec!["line2".into(), "line3".into()],
            }]
        );
    }

    #[test]
    fn swap_single_line() {
        let sec = single("[a#0000]\nSWAP 5:\n+new\n");
        assert_eq!(
            sec.ops,
            vec![Op::Swap {
                start: 5,
                end: 5,
                payload: vec!["new".into()],
            }]
        );
    }

    #[test]
    fn del_line_and_range() {
        let sec = single("[a#0000]\nDEL 4\nDEL 6.=8\n");
        assert_eq!(
            sec.ops,
            vec![Op::Del { start: 4, end: 4 }, Op::Del { start: 6, end: 8 },]
        );
    }

    #[test]
    fn ins_pre_post_head_tail() {
        let sec =
            single("[a#0000]\nINS.PRE 2:\n+p\nINS.POST 3:\n+q\nINS.HEAD:\n+h\nINS.TAIL:\n+t\n");
        assert_eq!(
            sec.ops,
            vec![
                Op::Ins {
                    cursor: Cursor::Pre(2),
                    payload: vec!["p".into()]
                },
                Op::Ins {
                    cursor: Cursor::Post(3),
                    payload: vec!["q".into()]
                },
                Op::Ins {
                    cursor: Cursor::Head,
                    payload: vec!["h".into()]
                },
                Op::Ins {
                    cursor: Cursor::Tail,
                    payload: vec!["t".into()]
                },
            ]
        );
    }

    #[test]
    fn block_ops_by_line() {
        let sec = single("[a#0000]\nSWAP.BLK 10:\n+body\nDEL.BLK 20\nINS.BLK.POST 30:\n+tail\n");
        assert_eq!(
            sec.ops,
            vec![
                Op::Block {
                    anchor: BlockAnchor::Line(10),
                    mode: BlockMode::Swap,
                    payload: vec!["body".into()]
                },
                Op::Block {
                    anchor: BlockAnchor::Line(20),
                    mode: BlockMode::Del,
                    payload: vec![]
                },
                Op::Block {
                    anchor: BlockAnchor::Line(30),
                    mode: BlockMode::InsPost,
                    payload: vec!["tail".into()]
                },
            ]
        );
    }

    #[test]
    fn block_ops_by_symbol() {
        let sec = single("[a#0000]\nSWAP.BLK #validate_token:\n+body\nDEL.BLK #old_fn\n");
        assert_eq!(
            sec.ops,
            vec![
                Op::Block {
                    anchor: BlockAnchor::Symbol("validate_token".into()),
                    mode: BlockMode::Swap,
                    payload: vec!["body".into()]
                },
                Op::Block {
                    anchor: BlockAnchor::Symbol("old_fn".into()),
                    mode: BlockMode::Del,
                    payload: vec![]
                },
            ]
        );
    }

    #[test]
    fn rem_and_mv() {
        let sec = single("[a#0000]\nREM\n");
        assert_eq!(sec.ops, vec![Op::Rem]);
        let sec = single("[a#0000]\nMV \"dst/new.rs\"\n");
        assert_eq!(
            sec.ops,
            vec![Op::Mv {
                dest: "dst/new.rs".into()
            }]
        );
    }

    #[test]
    fn multiple_sections() {
        let secs = parse_sections("[a#0001]\nDEL 1\n[b#0002]\nDEL 2\n").unwrap();
        assert_eq!(secs.len(), 2);
        assert_eq!(secs[0].path, "a");
        assert_eq!(secs[1].path, "b");
    }

    #[test]
    fn echoed_gutter_is_stripped_when_uniform() {
        // Model pasted back `N:content` read gutters on every row.
        let sec = single("[a#0000]\nSWAP 2.=3:\n2:let x = 1;\n3:let y = 2;\n");
        assert_eq!(
            sec.ops,
            vec![Op::Swap {
                start: 2,
                end: 3,
                payload: vec!["let x = 1;".into(), "let y = 2;".into()],
            }]
        );
    }

    #[test]
    fn genuine_numeric_body_not_stripped_when_mixed() {
        // A YAML-ish body where only some rows look like gutters must NOT be
        // stripped — the `12:` is real content.
        let sec = single("[a#0000]\nSWAP 1:\n+time: 12:30\n");
        assert_eq!(
            sec.ops,
            vec![Op::Swap {
                start: 1,
                end: 1,
                payload: vec!["time: 12:30".into()],
            }]
        );
    }

    #[test]
    fn plus_authored_numeric_gutter_survives_verbatim() {
        // `+`-authored rows are never bare: the `+` is stripped but a `N:`-shaped
        // payload (int-keyed map) is preserved verbatim, never gutter-stripped.
        let sec = single("[a#0000]\nSWAP 2.=3:\n+0: red\n+1: green\n");
        assert_eq!(
            sec.ops,
            vec![Op::Swap {
                start: 2,
                end: 3,
                payload: vec!["0: red".into(), "1: green".into()],
            }]
        );
    }

    #[test]
    fn bare_numeric_keyed_dict_not_stripped() {
        // Every stripped remainder is a lone quoted literal → numeric-keyed dict
        // shape, not read-output paste → the `N:` keys are left untouched
        // (matches oh-my-pi's allLiteralValues guard).
        let sec = single("[a#0000]\nSWAP 1.=2:\n0: \"one\"\n1: \"two\"\n");
        assert_eq!(
            sec.ops,
            vec![Op::Swap {
                start: 1,
                end: 2,
                payload: vec!["0: \"one\"".into(), "1: \"two\"".into()],
            }]
        );
    }

    #[test]
    fn bare_uniform_nonliteral_gutter_stripped() {
        // Bare rows all carry a `N:` gutter and the remainders are not lone
        // literals → read-output paste → strip the gutter (matches reference).
        let sec = single("[a#0000]\nSWAP 1.=2:\n0: red\n1: green\n");
        assert_eq!(
            sec.ops,
            vec![Op::Swap {
                start: 1,
                end: 2,
                payload: vec![" red".into(), " green".into()],
            }]
        );
    }

    #[test]
    fn malformed_range_is_structured_error_not_panic() {
        let err = parse_sections("[a#0000]\nSWAP 5.=2:\n+x\n").unwrap_err();
        assert_eq!(err.line, 2);
        assert!(err.msg.contains("ends before it starts"), "{}", err.msg);
    }

    #[test]
    fn payload_without_header_errors() {
        let err = parse_sections("[a#0000]\n+orphan payload\n").unwrap_err();
        assert_eq!(err.line, 2);
        assert!(err.msg.contains("no preceding op header"), "{}", err.msg);
    }

    #[test]
    fn del_with_body_errors() {
        let err = parse_sections("[a#0000]\nDEL 3:\n").unwrap_err();
        assert_eq!(err.line, 2);
        assert!(err.msg.contains("no colon"), "{}", err.msg);
    }

    #[test]
    fn op_without_section_header_errors() {
        let err = parse_sections("DEL 3\n").unwrap_err();
        assert_eq!(err.line, 1);
        assert!(err.msg.contains("no preceding"), "{}", err.msg);
    }

    #[test]
    fn empty_input_yields_no_sections() {
        assert_eq!(parse_sections("").unwrap(), vec![]);
    }
}
