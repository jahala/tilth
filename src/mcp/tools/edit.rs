use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::index::bloom::BloomFilterCache;
use crate::session::Session;

/// Parse one `files[]` entry. Parse errors are deferred onto the task so a
/// malformed entry surfaces as a per-file failure instead of aborting the
/// whole batch.
pub(in crate::mcp) fn parse_file_edit(index: usize, val: &Value) -> crate::edit::FileEditTask {
    use crate::edit::FileEditTask;

    let Some(path_str) = val.get("path").and_then(|v| v.as_str()) else {
        return FileEditTask::ParseError {
            label: format!("files[{index}]"),
            msg: "missing 'path'".into(),
        };
    };

    // Create shape: `create: true` + `content` at file level.
    // Strict boolean — the string `"true"` is rejected.
    let create_flag = val.get("create").and_then(Value::as_bool).unwrap_or(false);
    let content_field = val.get("content").and_then(|v| v.as_str());

    if create_flag {
        if val.get("edits").is_some() {
            return FileEditTask::ParseError {
                label: path_str.to_string(),
                msg: "cannot have both `create: true` and `edits` — use `create` to make a new file, `edits` to modify an existing one"
                    .into(),
            };
        }
        let Some(content) = content_field else {
            return FileEditTask::ParseError {
                label: path_str.to_string(),
                msg: "`create: true` requires `content` (the full file body, may be empty)".into(),
            };
        };
        return FileEditTask::Create {
            path: PathBuf::from(path_str),
            content: content.to_string(),
        };
    }

    // Edit shape. `content` at file level is only valid alongside `create: true`,
    // so surface that as a helpful error if it appears here.
    if content_field.is_some() {
        return FileEditTask::ParseError {
            label: path_str.to_string(),
            msg: "`content` at file level requires `create: true` — to modify an existing file, put content inside `edits`"
                .into(),
        };
    }

    let Some(edits_val) = val.get("edits").and_then(|v| v.as_array()) else {
        return FileEditTask::ParseError {
            label: path_str.to_string(),
            msg:
                "missing `edits` array (or set `create: true` with `content` to create a new file)"
                    .into(),
        };
    };

    if edits_val.is_empty() {
        return FileEditTask::ParseError {
            label: path_str.to_string(),
            msg: "'edits' array is empty — omit this file or add at least one edit".into(),
        };
    }

    let mut edits = Vec::with_capacity(edits_val.len());
    for (i, e) in edits_val.iter().enumerate() {
        match parse_edit_entry(i, e) {
            Ok(edit) => edits.push(edit),
            Err(msg) => {
                return FileEditTask::ParseError {
                    label: path_str.to_string(),
                    msg,
                };
            }
        }
    }

    FileEditTask::Ready {
        path: PathBuf::from(path_str),
        edits,
    }
}

/// Parse a single `edits[]` entry. Errors carry the edit index so the LLM
/// can fix exactly the right entry instead of guessing.
fn parse_edit_entry(i: usize, e: &Value) -> Result<crate::edit::Edit, String> {
    let start_str = e
        .get("start")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("edit[{i}]: missing 'start'"))?;
    let (start_line, start_hash) = crate::format::parse_anchor(start_str)
        .ok_or_else(|| format!("edit[{i}]: invalid start anchor '{start_str}'"))?;
    let (end_line, end_hash) = match e.get("end").and_then(|v| v.as_str()) {
        Some(end_str) => crate::format::parse_anchor(end_str)
            .ok_or_else(|| format!("edit[{i}]: invalid end anchor '{end_str}'"))?,
        None => (start_line, start_hash),
    };
    let content = e
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("edit[{i}]: missing 'content'"))?;
    Ok(crate::edit::Edit {
        start_line,
        start_hash,
        end_line,
        end_hash,
        content: content.to_string(),
    })
}

pub(in crate::mcp) fn tool_edit(
    args: &Value,
    session: &Session,
    bloom: &Arc<BloomFilterCache>,
) -> Result<String, String> {
    let files_val = args
        .get("files")
        .and_then(|v| v.as_array())
        .ok_or("missing required parameter: files (array of {path, edits})")?;

    if files_val.is_empty() {
        return Err("files array is empty".into());
    }
    if files_val.len() > 20 {
        return Err(format!(
            "batch edit limited to 20 files (got {})",
            files_val.len()
        ));
    }

    let show_diff = args
        .get("diff")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let tasks: Vec<crate::edit::FileEditTask> = files_val
        .iter()
        .enumerate()
        .map(|(i, v)| parse_file_edit(i, v))
        .collect();

    // Fast-fail on duplicates before touching session state. apply_batch
    // re-runs the same check as an encapsulation guarantee for any future
    // caller that bypasses this wire layer.
    if let Some(msg) = crate::edit::detect_duplicate_paths(&tasks) {
        return Err(msg);
    }

    for task in &tasks {
        match task {
            crate::edit::FileEditTask::Ready { path, .. }
            | crate::edit::FileEditTask::Create { path, .. } => {
                // Creating a file is a higher-commitment action than reading
                // one — the session must learn about created paths so the
                // hot-path heuristics and reads counter include them.
                session.record_read(path);
            }
            crate::edit::FileEditTask::ParseError { .. } => {}
        }
    }

    crate::edit::apply_batch(tasks, bloom, show_diff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_file_edit_rejects_empty_edits_array() {
        // Schema says minItems: 1, but schema validation is advisory — enforce
        // at runtime so a client that bypasses the schema can't silently get
        // a no-op success.
        let val = serde_json::json!({ "path": "noop.txt", "edits": [] });
        let task = parse_file_edit(0, &val);
        match task {
            crate::edit::FileEditTask::ParseError { label, msg } => {
                assert_eq!(label, "noop.txt");
                assert!(msg.contains("empty"), "unexpected msg: {msg}");
            }
            crate::edit::FileEditTask::Ready { .. } | crate::edit::FileEditTask::Create { .. } => {
                panic!("empty edits array should produce a ParseError");
            }
        }
    }

    // ── create-shape parser tests ─────────────────────────────────────────

    #[test]
    fn parse_create_with_content_produces_create_task() {
        let val = serde_json::json!({
            "path": "new.rs",
            "create": true,
            "content": "fn main() {}\n"
        });
        match parse_file_edit(0, &val) {
            crate::edit::FileEditTask::Create { path, content } => {
                assert_eq!(path, std::path::PathBuf::from("new.rs"));
                assert_eq!(content, "fn main() {}\n");
            }
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn parse_create_with_edits_rejects() {
        let val = serde_json::json!({
            "path": "x",
            "create": true,
            "content": "body",
            "edits": [{"start": "1:abc", "content": "x"}]
        });
        match parse_file_edit(0, &val) {
            crate::edit::FileEditTask::ParseError { msg, .. } => {
                assert!(
                    msg.contains("both") && msg.contains("create") && msg.contains("edits"),
                    "expected message to mention the mutex: {msg}"
                );
            }
            other => panic!("expected ParseError, got {other:?}"),
        }
    }

    #[test]
    fn parse_create_without_content_rejects() {
        let val = serde_json::json!({"path": "x", "create": true});
        match parse_file_edit(0, &val) {
            crate::edit::FileEditTask::ParseError { msg, .. } => {
                assert!(
                    msg.contains("requires") && msg.contains("content"),
                    "expected message to mention required content: {msg}"
                );
            }
            other => panic!("expected ParseError, got {other:?}"),
        }
    }

    #[test]
    fn parse_content_without_create_rejects() {
        let val = serde_json::json!({"path": "x", "content": "stray"});
        match parse_file_edit(0, &val) {
            crate::edit::FileEditTask::ParseError { msg, .. } => {
                assert!(
                    msg.contains("create: true") || msg.contains("`create`"),
                    "error should point the agent at the create flag: {msg}"
                );
            }
            other => panic!("expected ParseError, got {other:?}"),
        }
    }

    #[test]
    fn parse_create_false_with_edits_is_edit_shape() {
        // An explicit `create: false` alongside `edits` is redundant but not
        // wrong — treat it as the standard edit shape.
        let val = serde_json::json!({
            "path": "x.rs",
            "create": false,
            "edits": [{"start": "1:abc", "content": "x"}]
        });
        match parse_file_edit(0, &val) {
            crate::edit::FileEditTask::Ready { path, edits } => {
                assert_eq!(path, std::path::PathBuf::from("x.rs"));
                assert_eq!(edits.len(), 1);
            }
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn parse_create_as_string_does_not_trigger_create_shape() {
        // Strict boolean typing: `"create": "true"` (string) must NOT be
        // accepted as create. It falls through to the edit-shape path and
        // surfaces the regular "missing edits" error.
        let val = serde_json::json!({
            "path": "x.rs",
            "create": "true",
            "content": "irrelevant"
        });
        match parse_file_edit(0, &val) {
            crate::edit::FileEditTask::ParseError { msg, .. } => {
                // Either the "stray content" branch OR the "missing edits"
                // branch is acceptable here; both prove the create-shape path
                // didn't fire on a string.
                assert!(
                    msg.contains("content") || msg.contains("edits"),
                    "unexpected error: {msg}"
                );
            }
            other => panic!("string `create` should not accept create shape: {other:?}"),
        }
    }
}
