use std::fs;
use std::path::Path;

use crate::error::TilthError;
use crate::format;

/// A single edit operation targeting a line range by hash anchors.
#[derive(Debug, Clone)]
pub struct Edit {
    pub start_line: usize,
    pub start_hash: u16,
    pub end_line: usize,
    pub end_hash: u16,
    pub content: String,
}

/// Result of applying edits to a file.
#[derive(Debug)]
pub enum EditResult {
    /// All edits applied. Contains hashlined context around edit sites.
    Applied(String),
    /// One or more hashes didn't match current content.
    HashMismatch(String),
}

/// Apply a batch of edits to a file.
///
/// 1. Read file into lines
/// 2. Verify ALL hashes before applying ANY edit (fail-fast)
/// 3. Sort edits by `start_line` descending (reverse preserves line numbers)
/// 4. Splice replacements
/// 5. Write file
/// 6. Return hashlined context around edit sites
pub fn apply_edits(path: &Path, edits: &[Edit]) -> Result<EditResult, TilthError> {
    if edits.is_empty() {
        return Ok(EditResult::Applied(String::new()));
    }

    // Read file
    let content = fs::read_to_string(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => TilthError::NotFound {
            path: path.to_path_buf(),
            suggestion: None,
        },
        std::io::ErrorKind::PermissionDenied => TilthError::PermissionDenied {
            path: path.to_path_buf(),
        },
        _ => TilthError::IoError {
            path: path.to_path_buf(),
            source: e,
        },
    })?;

    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();

    // Phase 1: Verify all hashes
    let mut mismatches: Vec<String> = Vec::new();

    for edit in edits {
        // Bounds check
        if edit.start_line < 1 || edit.start_line > total {
            mismatches.push(format!(
                "Line {} out of bounds (file has {} lines)",
                edit.start_line, total
            ));
            continue;
        }
        if edit.end_line < 1 || edit.end_line > total {
            mismatches.push(format!(
                "Line {} out of bounds (file has {} lines)",
                edit.end_line, total
            ));
            continue;
        }
        if edit.end_line < edit.start_line {
            mismatches.push(format!(
                "Invalid range: {}-{} (end < start)",
                edit.start_line, edit.end_line
            ));
            continue;
        }

        // Verify start hash
        let start_idx = edit.start_line - 1;
        let start_actual_hash = format::line_hash(lines[start_idx].as_bytes());
        if start_actual_hash != edit.start_hash {
            let context_start = start_idx.saturating_sub(2);
            let context_end = (start_idx + 3).min(total);
            let context_lines: String = lines[context_start..context_end].join("\n");
            let hashlined = format::hashlines(&context_lines, (context_start + 1) as u32);
            mismatches.push(format!(
                "Hash mismatch at line {} (expected {:03x}, got {:03x}):\n{}",
                edit.start_line, edit.start_hash, start_actual_hash, hashlined
            ));
            continue;
        }

        // Verify end hash if different line
        if edit.end_line != edit.start_line {
            let end_idx = edit.end_line - 1;
            let end_actual_hash = format::line_hash(lines[end_idx].as_bytes());
            if end_actual_hash != edit.end_hash {
                let context_start = end_idx.saturating_sub(2);
                let context_end = (end_idx + 3).min(total);
                let context_lines: String = lines[context_start..context_end].join("\n");
                let hashlined = format::hashlines(&context_lines, (context_start + 1) as u32);
                mismatches.push(format!(
                    "Hash mismatch at line {} (expected {:03x}, got {:03x}):\n{}",
                    edit.end_line, edit.end_hash, end_actual_hash, hashlined
                ));
            }
        }
    }

    if !mismatches.is_empty() {
        return Ok(EditResult::HashMismatch(mismatches.join("\n\n")));
    }

    // Check for overlapping ranges
    let mut range_check: Vec<(usize, usize)> =
        edits.iter().map(|e| (e.start_line, e.end_line)).collect();
    range_check.sort_by_key(|&(s, _)| s);
    for pair in range_check.windows(2) {
        if pair[0].1 >= pair[1].0 {
            return Err(TilthError::InvalidQuery {
                query: format!(
                    "lines {}-{} and {}-{}",
                    pair[0].0, pair[0].1, pair[1].0, pair[1].1
                ),
                reason: "overlapping edit ranges in batch".into(),
            });
        }
    }

    // Phase 2: Apply edits in reverse order
    let mut indices: Vec<usize> = (0..edits.len()).collect();
    indices.sort_by_key(|&i| std::cmp::Reverse(edits[i].start_line));

    let mut owned: Vec<String> = lines.iter().map(|&s| s.to_string()).collect();

    for &idx in &indices {
        let edit = &edits[idx];
        let start_idx = edit.start_line - 1;
        let end_idx = edit.end_line; // exclusive end for inclusive range

        let replacement: Vec<String> = if edit.content.is_empty() {
            vec![]
        } else {
            edit.content.lines().map(String::from).collect()
        };

        owned.splice(start_idx..end_idx, replacement);
    }

    // Phase 3: Write file, preserving original line ending style
    let line_sep = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let has_trailing_newline = content.ends_with('\n');
    let mut output = owned.join(line_sep);
    if has_trailing_newline {
        output.push_str(line_sep);
    }

    fs::write(path, &output).map_err(|e| TilthError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;

    // Phase 4: Build response with context around each edit site.
    // Edits were applied in reverse order, so lower-numbered edits shift
    // the positions of higher-numbered ones. Track cumulative offset.
    let mut ctx_order: Vec<usize> = (0..edits.len()).collect();
    ctx_order.sort_by_key(|&i| edits[i].start_line);

    let mut offset: isize = 0;
    let mut contexts: Vec<String> = Vec::new();

    for &idx in &ctx_order {
        let edit = &edits[idx];
        let adjusted = ((edit.start_line as isize - 1) + offset).max(0) as usize;
        let old_count = edit.end_line - edit.start_line + 1;
        let new_count = if edit.content.is_empty() {
            0
        } else {
            edit.content.lines().count()
        };

        let context_start = adjusted.saturating_sub(5);
        let context_end = (adjusted + new_count + 5).min(owned.len());
        if context_start < context_end {
            let context_lines: String = owned[context_start..context_end].join("\n");
            let hashlined = format::hashlines(&context_lines, (context_start + 1) as u32);
            contexts.push(hashlined);
        }

        offset += new_count as isize - old_count as isize;
    }

    Ok(EditResult::Applied(contexts.join("\n---\n")))
}
