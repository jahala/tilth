//! JSON-native wire layer for `tilth_write`.
//!
//! The `edits` parameter is a JSON array of `{path, tag?, ops}` sections whose
//! ops are tag-discriminated on `op` (`snake_case`, pretrained-friendly names).
//! [`lower_edits`] deserializes that array and lowers it into the
//! grammar-independent [`Section`]/[`Op`] types in [`super::parser`], which
//! `apply.rs` and `block.rs` consume unchanged — the JSON shape is the only
//! thing that moved.
//!
//! The legacy `[path#TAG]` text grammar is no longer an accepted input. When a
//! caller passes the old blob (or double-encodes the array as a string), the
//! write boundary routes here for a teaching error that shows the corrected
//! JSON. [`super::parser::parse_sections`] survives *only* to render that
//! translation — it is error-path-only and not a supported input format.

use serde::Deserialize;
use serde_json::{json, Value};

use super::parser::{parse_sections, BlockAnchor, BlockMode, Cursor, Op, Section};
use super::tag::parse_tag;

/// One `{path, tag?, ops}` section as it arrives on the wire. `ops` stays as
/// raw `Value`s so each op's deserialize error can be tagged with its verb and
/// index before it bubbles up.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSection {
    path: String,
    #[serde(default)]
    tag: Option<String>,
    ops: Vec<Value>,
}

/// Wire-level op, tag-discriminated on `op`. `snake_case` names map 1:1 to the
/// grammar verbs (see [`lower_op`]).
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
enum JsonOp {
    Replace {
        start: u32,
        end: u32,
        content: String,
    },
    #[serde(rename = "replace_text")]
    TextSwap {
        old: String,
        new: String,
    },
    Delete {
        start: u32,
        end: u32,
    },
    InsertBefore {
        line: u32,
        content: String,
    },
    InsertAfter {
        line: u32,
        content: String,
    },
    Prepend {
        content: String,
    },
    Append {
        content: String,
    },
    ReplaceBlock {
        at: LineOrSymbol,
        content: String,
    },
    DeleteBlock {
        at: LineOrSymbol,
    },
    InsertAfterBlock {
        at: LineOrSymbol,
        content: String,
    },
    CreateFile {
        content: String,
    },
    DeleteFile,
    MoveFile {
        dest: String,
    },
}

/// A block anchor: an integer line, or a `"#symbol"` string. Untagged, so the
/// JSON scalar type disambiguates.
#[derive(Deserialize)]
#[serde(untagged)]
enum LineOrSymbol {
    Line(u32),
    Symbol(String),
}

impl LineOrSymbol {
    fn into_anchor(self) -> Result<BlockAnchor, String> {
        match self {
            LineOrSymbol::Line(n) => Ok(BlockAnchor::Line(n)),
            LineOrSymbol::Symbol(s) => {
                let trimmed = s.trim();
                let name = trimmed.strip_prefix('#').unwrap_or(trimmed).trim();
                if name.is_empty() {
                    return Err(format!("block anchor `at` {s:?} has an empty symbol name"));
                }
                Ok(BlockAnchor::Symbol(name.to_string()))
            }
        }
    }
}

/// Split a `content` string into the payload line vector the parser produces.
fn split_content(content: &str) -> Vec<String> {
    let mut rows: Vec<String> = content.split('\n').map(str::to_string).collect();
    // A trailing "" row (content ending in "\n") would splice an extra blank
    // line; the old grammar's finalize_payload stripped a trailing blank too.
    if rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    rows
}

fn lower_op(op: JsonOp) -> Result<Op, String> {
    Ok(match op {
        JsonOp::Replace {
            start,
            end,
            content,
        } => Op::Swap {
            start,
            end,
            payload: split_content(&content),
        },
        JsonOp::Delete { start, end } => Op::Del { start, end },
        JsonOp::TextSwap { old, new } => Op::TextSwap { old, new },
        JsonOp::InsertBefore { line, content } => Op::Ins {
            cursor: Cursor::Pre(line),
            payload: split_content(&content),
        },
        JsonOp::InsertAfter { line, content } => Op::Ins {
            cursor: Cursor::Post(line),
            payload: split_content(&content),
        },
        JsonOp::Prepend { content } => Op::Ins {
            cursor: Cursor::Head,
            payload: split_content(&content),
        },
        JsonOp::Append { content } => Op::Ins {
            cursor: Cursor::Tail,
            payload: split_content(&content),
        },
        JsonOp::ReplaceBlock { at, content } => Op::Block {
            anchor: at.into_anchor()?,
            mode: BlockMode::Swap,
            payload: split_content(&content),
        },
        JsonOp::DeleteBlock { at } => Op::Block {
            anchor: at.into_anchor()?,
            mode: BlockMode::Del,
            payload: Vec::new(),
        },
        JsonOp::InsertAfterBlock { at, content } => Op::Block {
            anchor: at.into_anchor()?,
            mode: BlockMode::InsPost,
            payload: split_content(&content),
        },
        JsonOp::CreateFile { content } => Op::Create { content },
        JsonOp::DeleteFile => Op::Rem,
        JsonOp::MoveFile { dest } => Op::Mv { dest },
    })
}

fn lower_section(index: usize, raw: RawSection) -> Result<Section, String> {
    let tag = match raw.tag {
        None => None,
        Some(t) => {
            let stripped = t.strip_prefix('#').unwrap_or(t.as_str());
            Some(parse_tag(stripped).ok_or_else(|| {
                format!(
                    "edits[{index}] (path {:?}): tag {t:?} is not a 4-hex-digit tag from an \
                     edit-mode read — run an edit-mode tilth_read of the path to mint a fresh tag",
                    raw.path
                )
            })?)
        }
    };
    let mut ops = Vec::with_capacity(raw.ops.len());
    for (j, ov) in raw.ops.into_iter().enumerate() {
        let verb = ov
            .get("op")
            .and_then(Value::as_str)
            .unwrap_or("<missing>")
            .to_string();
        let parsed: JsonOp = serde_json::from_value(ov).map_err(|e| {
            format!(
                "edits[{index}].ops[{j}] op \"{verb}\": {e}. Example op: \
                 {{\"op\":\"replace\",\"start\":12,\"end\":14,\"content\":\"...\"}} — ops are \
                 line-addressed using line numbers from the tagged read — not find/replace."
            )
        })?;
        ops.push(
            lower_op(parsed).map_err(|e| format!("edits[{index}].ops[{j}] op \"{verb}\": {e}"))?,
        );
    }
    Ok(Section {
        path: raw.path,
        tag,
        ops,
    })
}

/// Deserialize the `edits` array into lowered [`Section`]s. Any structural or
/// per-op failure returns an `Err` naming the section index, the op verb, and
/// the offending field — surfaced before any file is touched.
pub fn lower_edits(edits: &Value) -> Result<Vec<Section>, String> {
    let arr = edits
        .as_array()
        .ok_or("`edits` must be a JSON array of {path, tag?, ops} section objects")?;
    // Enforce the batch cap up front, before allocating or deserializing any
    // section — an oversized array is rejected without paying to lower it.
    if arr.len() > 20 {
        return Err(format!(
            "batch write limited to 20 sections (got {})",
            arr.len()
        ));
    }
    let mut sections = Vec::with_capacity(arr.len());
    for (i, sv) in arr.iter().enumerate() {
        let raw: RawSection =
            serde_json::from_value(sv.clone()).map_err(|e| format!("edits[{i}]: {e}"))?;
        sections.push(lower_section(i, raw)?);
    }
    Ok(sections)
}

/// Build a teaching error for an `edits` value that arrived as a string instead
/// of a JSON array — either the legacy `[path#TAG]` blob or a double-encoded
/// JSON payload. The message shows the corrected JSON form.
pub fn teaching_error_for_string(s: &str) -> String {
    // Double-encoding: the string body is itself valid JSON (array or object).
    // A legacy blob like "[src/x.rs#1A2B]\n..." starts with '[' but is NOT
    // valid JSON, so it falls through to the legacy branch.
    if let Ok(inner) = serde_json::from_str::<Value>(s) {
        if inner.is_array() || inner.is_object() {
            let pretty = serde_json::to_string_pretty(&inner).unwrap_or_default();
            return format!(
                "`edits` was passed as a JSON-encoded string (double-encoded). Pass the array \
                 itself as the `edits` value, not a string containing JSON. Unwrapped form:\n{pretty}"
            );
        }
    }

    // Legacy blob: parse it read-only and render the equivalent JSON array.
    match parse_sections(s) {
        Ok(sections) if !sections.is_empty() => {
            let translated = render_sections_as_json(&sections);
            let pretty = serde_json::to_string_pretty(&translated).unwrap_or_default();
            format!(
                "`edits` is now a JSON array of {{path, tag?, ops}} sections, not the `[path#TAG]` \
                 text grammar. Your input translated to the new form:\n{pretty}"
            )
        }
        _ => "`edits` must be a JSON array of {path, tag?, ops} section objects, not a string."
            .to_string(),
    }
}

/// Render lowered [`Section`]s back into the JSON `edits` shape. Error-path only
/// — used solely to translate a legacy blob inside [`teaching_error_for_string`].
fn render_sections_as_json(sections: &[Section]) -> Value {
    let arr: Vec<Value> = sections
        .iter()
        .map(|s| {
            let mut obj = serde_json::Map::new();
            obj.insert("path".into(), json!(s.path));
            if let Some(tag) = s.tag {
                obj.insert("tag".into(), json!(format!("{tag:04X}")));
            }
            obj.insert(
                "ops".into(),
                Value::Array(s.ops.iter().map(render_op_as_json).collect()),
            );
            Value::Object(obj)
        })
        .collect();
    Value::Array(arr)
}

fn anchor_to_json(anchor: &BlockAnchor) -> Value {
    match anchor {
        BlockAnchor::Line(n) => json!(n),
        BlockAnchor::Symbol(name) => json!(format!("#{name}")),
    }
}

fn render_op_as_json(op: &Op) -> Value {
    match op {
        Op::Swap {
            start,
            end,
            payload,
        } => json!({
            "op": "replace", "start": start, "end": end, "content": payload.join("\n"),
        }),
        Op::TextSwap { old, new } => {
            json!({ "op": "replace_text", "old": old, "new": new })
        }
        Op::Del { start, end } => json!({ "op": "delete", "start": start, "end": end }),
        Op::Ins { cursor, payload } => {
            let content = payload.join("\n");
            match cursor {
                Cursor::Pre(n) => json!({ "op": "insert_before", "line": n, "content": content }),
                Cursor::Post(n) => json!({ "op": "insert_after", "line": n, "content": content }),
                Cursor::Head => json!({ "op": "prepend", "content": content }),
                Cursor::Tail => json!({ "op": "append", "content": content }),
            }
        }
        Op::Block {
            anchor,
            mode,
            payload,
        } => {
            let at = anchor_to_json(anchor);
            match mode {
                BlockMode::Swap => {
                    json!({ "op": "replace_block", "at": at, "content": payload.join("\n") })
                }
                BlockMode::Del => json!({ "op": "delete_block", "at": at }),
                BlockMode::InsPost => {
                    json!({ "op": "insert_after_block", "at": at, "content": payload.join("\n") })
                }
            }
        }
        Op::Create { content } => json!({ "op": "create_file", "content": content }),
        Op::Rem => json!({ "op": "delete_file" }),
        Op::Mv { dest } => json!({ "op": "move_file", "dest": dest }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowers_replace_into_swap_with_split_payload() {
        let edits = json!([{
            "path": "a.rs",
            "tag": "1A2B",
            "ops": [{ "op": "replace", "start": 1, "end": 2, "content": "x\ny" }]
        }]);
        let sections = lower_edits(&edits).expect("lowers");
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].path, "a.rs");
        assert_eq!(sections[0].tag, Some(0x1A2B));
        assert_eq!(
            sections[0].ops,
            vec![Op::Swap {
                start: 1,
                end: 2,
                payload: vec!["x".into(), "y".into()],
            }]
        );
    }

    #[test]
    fn absent_tag_lowers_to_none_for_new_file_seed() {
        let edits = json!([{
            "path": "new.rs",
            "ops": [{ "op": "prepend", "content": "fn seeded() {}" }]
        }]);
        let sections = lower_edits(&edits).expect("lowers");
        assert_eq!(sections[0].tag, None);
        assert_eq!(
            sections[0].ops,
            vec![Op::Ins {
                cursor: Cursor::Head,
                payload: vec!["fn seeded() {}".into()],
            }]
        );
    }

    #[test]
    fn block_symbol_anchor_strips_hash() {
        let edits = json!([{
            "path": "a.rs", "tag": "0000",
            "ops": [{ "op": "delete_block", "at": "#handleAuth" }]
        }]);
        let sections = lower_edits(&edits).expect("lowers");
        assert_eq!(
            sections[0].ops,
            vec![Op::Block {
                anchor: BlockAnchor::Symbol("handleAuth".into()),
                mode: BlockMode::Del,
                payload: Vec::new(),
            }]
        );
    }

    #[test]
    fn block_line_anchor_lowers_to_line() {
        let edits = json!([{
            "path": "a.rs", "tag": "0000",
            "ops": [{ "op": "replace_block", "at": 42, "content": "fn x() {}" }]
        }]);
        let sections = lower_edits(&edits).expect("lowers");
        assert_eq!(
            sections[0].ops,
            vec![Op::Block {
                anchor: BlockAnchor::Line(42),
                mode: BlockMode::Swap,
                payload: vec!["fn x() {}".into()],
            }]
        );
    }

    #[test]
    fn missing_op_field_error_names_op_and_field() {
        let edits = json!([{
            "path": "a.rs", "tag": "0000",
            "ops": [{ "op": "replace", "start": 1, "end": 2 }]
        }]);
        let err = lower_edits(&edits).expect_err("missing content must fail");
        assert!(err.contains("replace"), "must name the op: {err}");
        assert!(err.contains("content"), "must name the field: {err}");
    }

    #[test]
    fn bad_tag_error_names_the_tag() {
        let edits = json!([{
            "path": "a.rs", "tag": "ZZZZ",
            "ops": [{ "op": "delete", "start": 1, "end": 1 }]
        }]);
        let err = lower_edits(&edits).expect_err("bad hex tag must fail");
        assert!(err.contains("ZZZZ") && err.contains("hex"), "got: {err}");
    }

    #[test]
    fn non_array_edits_rejected() {
        let err = lower_edits(&json!({"path": "a.rs"})).expect_err("object is not an array");
        assert!(err.contains("must be a JSON array"), "got: {err}");
    }

    #[test]
    fn unknown_field_on_op_is_rejected() {
        // A stray `content` on a `delete` op (or a typo'd key) must surface a
        // named deserialize error, not be silently dropped.
        let edits = json!([{
            "path": "a.rs", "tag": "0000",
            "ops": [{ "op": "delete", "start": 1, "end": 1, "content": "oops" }]
        }]);
        let err = lower_edits(&edits).expect_err("unknown field must fail");
        assert!(err.contains("delete"), "must name the op: {err}");
        assert!(
            err.contains("content"),
            "must name the offending field: {err}"
        );
    }

    #[test]
    fn legacy_blob_teaching_error_shows_json_translation() {
        let blob = "[src/x.rs#1A2B]\nSWAP 2:\n+let y = 1;\nDEL 5\n";
        let err = teaching_error_for_string(blob);
        assert!(
            err.contains("JSON array"),
            "must teach the new shape: {err}"
        );
        assert!(
            err.contains("\"op\": \"replace\""),
            "must render the swap as a replace op: {err}"
        );
        assert!(
            err.contains("\"op\": \"delete\""),
            "must render the DEL as a delete op: {err}"
        );
        assert!(err.contains("src/x.rs"), "must carry the path: {err}");
    }

    #[test]
    fn double_encoded_array_teaching_error_shows_unwrapped_form() {
        let inner = json!([{ "path": "a.rs", "tag": "0000", "ops": [] }]);
        let encoded = serde_json::to_string(&inner).unwrap();
        let err = teaching_error_for_string(&encoded);
        assert!(
            err.contains("double-encoded"),
            "must name the mistake: {err}"
        );
        assert!(
            err.contains("\"path\": \"a.rs\""),
            "must show unwrapped form: {err}"
        );
    }

    #[test]
    fn op_deserialize_failure_teaches_example_and_line_addressed() {
        let cases = [
            json!([{ "path": "a.rs", "tag": "0000", "ops": [{ "op": "delete", "start": 1, "end": 1, "content": "oops" }] }]),
            json!([{ "path": "a.rs", "tag": "0000", "ops": [{ "op": "seed" }] }]),
            json!([{ "path": "a.rs", "tag": "0000", "ops": [{ "op": "replace", "start": 1, "end": 2 }] }]),
        ];
        for edits in cases {
            let err = lower_edits(&edits).expect_err("bad op must fail");
            assert!(
                err.contains(r#"{"op":"replace","start":12,"end":14,"content":"..."}"#),
                "must show the canonical example op: {err}"
            );
            assert!(
                err.contains("line-addressed"),
                "must teach line-addressing, not find/replace: {err}"
            );
        }
    }

    #[test]
    fn leading_hash_tag_strips_and_passes() {
        let edits = json!([{
            "path": "a.rs", "tag": "#A0EA",
            "ops": [{ "op": "delete", "start": 1, "end": 1 }]
        }]);
        let sections = lower_edits(&edits).expect("leading # must strip and parse");
        assert_eq!(sections[0].tag, Some(0xA0EA));
    }

    #[test]
    fn leading_hash_wrong_tag_still_fails() {
        let edits = json!([{
            "path": "a.rs", "tag": "#ZZZZ",
            "ops": [{ "op": "delete", "start": 1, "end": 1 }]
        }]);
        let err = lower_edits(&edits).expect_err("stripped non-hex tag must still fail");
        assert!(err.contains("ZZZZ") && err.contains("hex"), "got: {err}");
    }

    #[test]
    fn bad_tag_error_hints_mint_a_tag() {
        let edits = json!([{
            "path": "a.rs", "tag": "auto",
            "ops": [{ "op": "delete", "start": 1, "end": 1 }]
        }]);
        let err = lower_edits(&edits).expect_err("bad tag must fail");
        assert!(
            err.contains("tilth_read") && err.contains("mint"),
            "must hint how to mint a fresh tag: {err}"
        );
    }

    #[test]
    fn parses_replace_text_op() {
        let edits = json!([{
            "path": "a.rs",
            "tag": "1A2B",
            "ops": [{ "op": "replace_text", "old": "target", "new": "replacement" }]
        }]);
        let sections = lower_edits(&edits).expect("replace_text lowers");
        assert_eq!(
            sections[0].ops,
            vec![Op::TextSwap {
                old: "target".into(),
                new: "replacement".into()
            }]
        );
    }
}
