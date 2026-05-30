use std::fmt::Write as _;
use std::path::PathBuf;

use serde_json::Value;

use crate::cache::OutlineCache;
use crate::session::Session;

use super::apply_budget;

pub(in crate::mcp) fn tool_read(
    args: &Value,
    cache: &OutlineCache,
    session: &Session,
    edit_mode: bool,
) -> Result<String, String> {
    let budget = args.get("budget").and_then(serde_json::Value::as_u64);

    // Batch-only API: `paths` is required — pass a single-element array to read
    // one file. The singular `path` parameter was removed; accepting both made
    // agents guess which one each call wanted.
    let paths_arr = args.get("paths").and_then(|v| v.as_array()).ok_or(
        "missing required parameter: paths (array of file paths; use a single-element array to read one file)",
    )?;
    if paths_arr.is_empty() {
        return Err("paths must contain at least one file path".into());
    }
    if paths_arr.len() > 20 {
        return Err(format!(
            "batch read limited to 20 files (got {})",
            paths_arr.len()
        ));
    }

    let section = args.get("section").and_then(|v| v.as_str());
    let sections_arr = args.get("sections").and_then(|v| v.as_array());
    let full = args
        .get("full")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    // Multi-file batch read (capped at 20 to bound I/O). section/sections/full
    // are single-file controls — reject them rather than silently ignore.
    if paths_arr.len() > 1 {
        if section.is_some() || sections_arr.is_some() || full {
            return Err(
                "section / sections / full apply to a single file — pass exactly one path in `paths` to use them"
                    .into(),
            );
        }

        // Aggregate deadline for batch reads: 60s default, override with TILTH_BATCH_TIMEOUT
        // Note: deadline is checked between files, so a single massive file could still
        // exceed it. The per-request timeout (handle_tool_call) catches that case.
        let batch_timeout = std::env::var("TILTH_BATCH_TIMEOUT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(60);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(batch_timeout);

        let mut results = Vec::with_capacity(paths_arr.len());
        for (i, p) in paths_arr.iter().enumerate() {
            // Check deadline before each file
            if std::time::Instant::now() > deadline {
                results.push(format!(
                    "# batch read stopped — deadline exceeded after {}/{} files. \
                     Reduce batch size or set TILTH_BATCH_TIMEOUT=<seconds>.",
                    i,
                    paths_arr.len()
                ));
                break;
            }

            let path_str = p.as_str().ok_or("paths must be an array of strings")?;
            let path = PathBuf::from(path_str);
            session.record_read(&path);
            match crate::read::read_file(&path, None, false, cache, edit_mode) {
                Ok(output) => results.push(output),
                Err(e) => results.push(format!("# {} — error: {}", path.display(), e)),
            }
        }
        let combined = results.join("\n\n");
        return Ok(apply_budget(combined, budget));
    }

    // Single-file read (`paths` has exactly one entry).
    let path_str = paths_arr[0]
        .as_str()
        .ok_or("paths must be an array of strings")?;
    let path = PathBuf::from(path_str);

    if section.is_some() && sections_arr.is_some() {
        return Err("provide either section (single) or sections (array), not both".into());
    }

    // Multi-section path: bypass smart view + related-file hints (those only
    // apply to whole-file reads).
    if let Some(arr) = sections_arr {
        let ranges: Vec<&str> = arr
            .iter()
            .map(|v| v.as_str().ok_or("sections must be an array of strings"))
            .collect::<Result<Vec<_>, _>>()?;
        if ranges.is_empty() {
            return Err("sections must contain at least one range".into());
        }
        if ranges.len() > 20 {
            return Err(format!(
                "sections limited to 20 per call (got {})",
                ranges.len()
            ));
        }
        session.record_read(&path);
        let output =
            crate::read::read_ranges(&path, &ranges, edit_mode).map_err(|e| e.to_string())?;
        return Ok(apply_budget(output, budget));
    }

    session.record_read(&path);
    let mut output = crate::read::read_file(&path, section, full, cache, edit_mode)
        .map_err(|e| e.to_string())?;

    // Append related-file hint for outlined code files (not section reads, not batch).
    if section.is_none() && crate::read::would_outline(&path) {
        let related = crate::read::imports::resolve_related_files(&path);
        if !related.is_empty() {
            output.push_str("\n\n> Related: ");
            for (i, p) in related.iter().enumerate() {
                if i > 0 {
                    output.push_str(", ");
                }
                let _ = write!(output, "{}", p.display());
            }
        }
    }

    Ok(apply_budget(output, budget))
}
