//! `tilth_read` — batched file reads with smart view, suffix grammar
//! (`#n-m` / `#n` / `#heading` / `#symbol`), mode override
//! (`auto` / `full` / `signature` / `stripped`), and `if_modified_since` headers.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::cache::OutlineCache;
use crate::mcp::path_suffix::PathSuffix;
use crate::session::Session;

pub(crate) fn tool_read(
    args: &Value,
    cache: &OutlineCache,
    session: &Session,
    edit_mode: bool,
) -> Result<String, String> {
    let budget = args.get("budget").and_then(serde_json::Value::as_u64);

    // Accept singular `path:` form (82% of agents use it; not worth fighting)
    // alongside the documented `paths: [...]` array.
    let paths_arr_owned: Vec<Value>;
    let paths_arr: &Vec<Value> = match args.get("paths") {
        Some(v) => v.as_array().ok_or(
            "paths must be an array of file paths (use single-element array for one file)",
        )?,
        None => match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => {
                paths_arr_owned = vec![Value::String(p.to_string())];
                &paths_arr_owned
            }
            None => {
                return Err("missing required parameter: paths (array of file paths)".into());
            }
        },
    };

    if paths_arr.is_empty() {
        return Err("paths must contain at least one file".into());
    }
    if paths_arr.len() > 20 {
        return Err(format!(
            "batch read limited to 20 files (got {})",
            paths_arr.len()
        ));
    }

    let raw_paths: Vec<String> = paths_arr
        .iter()
        .map(|p| {
            p.as_str()
                .ok_or("paths must be an array of strings")
                .map(String::from)
        })
        .collect::<Result<_, _>>()?;

    // `mode: auto|full|signature|stripped` overrides the implicit smart-view.
    let mode_str = args.get("mode").and_then(|v| v.as_str()).unwrap_or("auto");
    if !matches!(mode_str, "auto" | "full" | "signature" | "stripped") {
        return Err(format!(
            "unknown read mode: {mode_str}. Use: auto, full, signature, stripped"
        ));
    }
    let force_full = mode_str == "full";
    let force_signature = mode_str == "signature";
    let force_stripped = mode_str == "stripped";

    // if_modified_since: skip files whose mtime is <= ts (return stub).
    let since = args
        .get("if_modified_since")
        .and_then(|v| v.as_str())
        .and_then(crate::mcp::iso::parse_iso_utc);

    // Resolve suffix grammar on each path spec into (PathBuf, Suffix)
    let parsed: Vec<(PathBuf, PathSuffix)> = raw_paths
        .iter()
        .map(|s| crate::mcp::path_suffix::parse_path_with_suffix(s))
        .collect();
    let paths: Vec<PathBuf> = parsed.iter().map(|(p, _)| p.clone()).collect();
    let suffixes: Vec<&PathSuffix> = parsed.iter().map(|(_, s)| s).collect();

    let now = std::time::SystemTime::now();
    let has_any_suffix = suffixes.iter().any(|s| !matches!(s, PathSuffix::None));

    // Multi-file batch: per-file smart view applies, but no related-file hints
    // (those only make sense for whole-file reads of a single target).
    if paths.len() > 1 {
        if has_any_suffix
            || since.is_some()
            || force_signature
            || force_stripped
            || force_full
            || mode_str == "auto"
        {
            // Per-path resolution so suffix/since/signature behave correctly.
            let mut parts: Vec<String> = Vec::with_capacity(paths.len());
            for (path, suffix) in &parsed {
                session.record_read(path);
                if let Some(s_ts) = since {
                    if !crate::mcp::iso::file_changed_since(path, s_ts) {
                        parts.push(crate::mcp::iso::unchanged_stub(path, s_ts));
                        continue;
                    }
                }
                let signature = force_signature
                    || (!force_full
                        && mode_str == "auto"
                        && matches!(suffix, PathSuffix::None)
                        && should_auto_signature(path));
                let body = if force_full && matches!(suffix, PathSuffix::None) {
                    crate::read::read_file(path, None, true, cache, edit_mode)
                        .unwrap_or_else(|e| format!("# {}\nerror: {}", path.display(), e))
                } else {
                    read_single_with_suffix(
                        path,
                        suffix,
                        signature,
                        force_stripped,
                        edit_mode,
                        cache,
                    )
                };
                parts.push(body);
            }
            let combined = parts.join("\n\n");
            // Multi-file responses don't carry per-file view-meta — the agent
            // can read each per-file `# path (...) [mode]` header inline.
            return Ok(finalize_response(
                Some(now),
                serde_json::Map::new(),
                combined,
                budget,
            ));
        }
        let combined = crate::read::read_batch(&paths, cache, session, edit_mode);
        return Ok(super::apply_budget(combined, budget));
    }

    let path = paths.into_iter().next().expect("paths non-empty");
    let suffix = suffixes
        .into_iter()
        .next()
        .cloned()
        .unwrap_or(PathSuffix::None);

    // if_modified_since on a single path
    if let Some(s_ts) = since {
        if !crate::mcp::iso::file_changed_since(&path, s_ts) {
            let body = crate::mcp::iso::unchanged_stub(&path, s_ts);
            return Ok(finalize_response(
                Some(now),
                serde_json::Map::new(),
                body,
                None,
            ));
        }
    }

    // Path-suffix grammar drives slicing; standalone `section`/`sections` were
    // removed per spec AC-5. Both `mode=signature` and `mode=stripped` are
    // whole-file shape modes — a suffix narrows to a specific range, so the
    // mode flags are dropped here in favor of the explicit slice the LLM asked for.
    if !matches!(suffix, PathSuffix::None) {
        session.record_read(&path);
        let body =
            read_single_with_suffix(&path, &suffix, force_signature, false, edit_mode, cache);
        // Suffix-driven reads carry no view-meta — the LLM declared the slice.
        // Cache token still rides along when if_modified_since was supplied.
        return Ok(finalize_response(
            since.map(|_| now),
            serde_json::Map::new(),
            body,
            budget,
        ));
    }

    session.record_read(&path);

    let auto_signature_promotion =
        !force_full && !force_stripped && mode_str == "auto" && should_auto_signature(&path);

    // `mode: signature` (explicit or auto-promoted) is an outline-style read.
    if force_signature || auto_signature_promotion {
        return respond_signature(&path, cache, auto_signature_promotion, budget);
    }

    // `mode: stripped` is comment/log-stripped; explicit only (no auto path).
    // Explicit shape request → no `next_view` hint, matching `mode=signature`.
    if force_stripped {
        return respond_stripped(&path, cache, budget);
    }

    let mut output = crate::read::read_file(&path, None, force_full, cache, edit_mode)
        .map_err(|e| e.to_string())?;

    // Append related-file hint for outlined code files.
    if crate::read::would_outline(&path) {
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

    // `mode=auto` on a large non-code file routes through `read_file` and
    // emits an outline (markdown headings, JSON keys). Signal that the LLM
    // got less than the full content so it can escalate to `mode=full`.
    let mut meta = serde_json::Map::new();
    if !force_full && crate::read::would_outline(&path) {
        meta.insert("view".into(), Value::String("outline".into()));
        if let Some(total) = count_lines(&path) {
            meta.insert("original_line_count".into(), Value::from(total));
        }
        meta.insert("next_view".into(), Value::String("full".into()));
    }

    Ok(finalize_response(None, meta, output, budget))
}

/// Resolve a single path+suffix to its read output. `signature` and
/// `stripped` are whole-file shape modes; both are honored only for
/// `PathSuffix::None` (any explicit suffix wins). They are mutually
/// exclusive — `signature` takes precedence if both happen to be set.
pub(crate) fn read_single_with_suffix(
    path: &Path,
    suffix: &PathSuffix,
    signature: bool,
    stripped: bool,
    edit_mode: bool,
    cache: &OutlineCache,
) -> String {
    let render_err = |e: crate::error::TilthError| format!("# {}\nerror: {}", path.display(), e);
    match suffix {
        PathSuffix::LineRange(s, e) => {
            let range = format!("{s}-{e}");
            crate::read::read_ranges(path, &[range.as_str()], edit_mode).unwrap_or_else(render_err)
        }
        PathSuffix::FromLine(n) => {
            // Resolve total lines via metadata + count; cheap & avoids full read.
            let total = count_lines(path).map_or(*n, |t| t as usize);
            let range = format!("{n}-{}", total.max(*n));
            crate::read::read_ranges(path, &[range.as_str()], edit_mode).unwrap_or_else(render_err)
        }
        PathSuffix::Heading(h) => {
            crate::read::read_ranges(path, &[h.as_str()], edit_mode).unwrap_or_else(render_err)
        }
        PathSuffix::Symbol(name) => {
            // Resolve symbol via outline → range, then read that range.
            match resolve_symbol_range(path, name) {
                Some((s, e)) => {
                    let range = format!("{s}-{e}");
                    crate::read::read_ranges(path, &[range.as_str()], edit_mode)
                        .unwrap_or_else(render_err)
                }
                None => {
                    format!(
                        "# {}\nerror: symbol '{}' not found in outline",
                        path.display(),
                        name
                    )
                }
            }
        }
        PathSuffix::None => {
            if signature {
                // Multi-file batch path: discard the line count — view-meta is
                // not emitted for multi-file responses.
                return read_signature_file(path, cache).map_or_else(render_err, |(body, _)| body);
            }
            if stripped {
                return read_stripped_file(path, cache)
                    .map_or_else(render_err, |(body, _, _)| body);
            }
            crate::read::read_file(path, None, false, cache, edit_mode).unwrap_or_else(render_err)
        }
    }
}

fn find_symbol_entry(entries: &[crate::types::OutlineEntry], name: &str) -> Option<(usize, usize)> {
    for e in entries {
        if e.name == name {
            return Some((e.start_line as usize, e.end_line as usize));
        }
        if let Some(hit) = find_symbol_entry(&e.children, name) {
            return Some(hit);
        }
    }
    None
}

/// Look up `name` in the file's outline; return its 1-indexed `(start, end)`.
fn resolve_symbol_range(path: &Path, name: &str) -> Option<(usize, usize)> {
    let content = std::fs::read_to_string(path).ok()?;
    let ft = crate::lang::detect_file_type(path);
    let crate::types::FileType::Code(lang) = ft else {
        return None;
    };
    let entries = crate::lang::outline::get_outline_entries(&content, lang);
    find_symbol_entry(&entries, name)
}

fn should_auto_signature(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !matches!(
        crate::lang::detect_file_type(path),
        crate::types::FileType::Code(_)
    ) {
        return false;
    }
    crate::types::estimate_tokens(meta.len()) > crate::read::TOKEN_THRESHOLD
}

/// Cheap line count for view-meta. Returns `None` when the file can't be
/// read; OS page cache makes the second read effectively free when a helper
/// already touched the file moments earlier. Byte-level newline scan avoids
/// allocating a UTF-8 `String` just to count `\n` (and tolerates non-UTF-8
/// content, which `read_to_string` would reject).
fn count_lines(path: &Path) -> Option<u32> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.is_empty() {
        return Some(0);
    }
    let nl = memchr::memchr_iter(b'\n', &bytes).count();
    let total = if bytes.last() == Some(&b'\n') {
        nl
    } else {
        nl + 1
    };
    Some(u32::try_from(total).unwrap_or(u32::MAX))
}

/// Apply budget to `body`, then prepend a single JSON header line that
/// combines the optional cache token (`now`) with any view-shape `meta`
/// fields the caller built (`view`, `original_line_count`, `next_view`,
/// `lines_stripped`, ...). If the budget actually clipped the body, the
/// truncation fields (`truncated`, `truncated_at_line`,
/// `original_line_count`) are merged into the same meta object before the
/// header is rendered so the agent only ever parses one line.
///
/// Budget accounting has two layers: this function subtracts the JSON
/// header's estimated tokens (plus a 16-token pad for the truncation
/// fields that may be added later) from the user-requested budget before
/// calling `apply_with_info`. `apply_with_info` then reserves an additional
/// 50 tokens internally for the body's own `# path (...)` header line.
/// Net: user budget covers the rendered response including both headers.
fn finalize_response(
    now: Option<SystemTime>,
    mut meta: serde_json::Map<String, Value>,
    body: String,
    budget: Option<u64>,
) -> String {
    let body_budget = budget.map(|b| {
        let header_preview = crate::mcp::iso::with_meta_header(now, meta.clone(), "");
        let header_tokens = crate::types::estimate_tokens(header_preview.len() as u64);
        b.saturating_sub(header_tokens + 16)
    });
    let (body_final, info) = match body_budget {
        Some(b) => crate::budget::apply_with_info(&body, b),
        None => (body, None),
    };
    if let Some(info) = info {
        meta.insert("truncated".into(), Value::Bool(true));
        meta.insert("truncated_at_line".into(), Value::from(info.at_line));
        // Don't clobber an already-set original_line_count (the caller may have
        // set a more accurate value from a non-budgeted source).
        meta.entry("original_line_count")
            .or_insert_with(|| Value::from(info.original_line_count));
    }
    crate::mcp::iso::with_meta_header(now, meta, &body_final)
}

/// Single-file `mode=signature` (explicit or auto-promoted). Builds the
/// view-meta object and routes through `finalize_response` so budget +
/// header accounting stay in one place.
fn respond_signature(
    path: &Path,
    cache: &OutlineCache,
    auto_promotion: bool,
    budget: Option<u64>,
) -> Result<String, String> {
    let (body, total_lines) = read_signature_file(path, cache).map_err(|e| e.to_string())?;
    let mut meta = serde_json::Map::new();
    meta.insert("view".into(), Value::String("signature".into()));
    meta.insert("original_line_count".into(), Value::from(total_lines));
    // Implicit promotion from `mode=auto` advertises the escalation path.
    // Explicit `mode=signature` doesn't — the LLM picked this view on purpose.
    if auto_promotion {
        meta.insert("next_view".into(), Value::String("full".into()));
    }
    Ok(finalize_response(None, meta, body, budget))
}

/// Single-file `mode=stripped`. Explicit shape request, so no `next_view`
/// hint — same contract as explicit `mode=signature`.
fn respond_stripped(
    path: &Path,
    cache: &OutlineCache,
    budget: Option<u64>,
) -> Result<String, String> {
    let (body, total_lines, lines_stripped) =
        read_stripped_file(path, cache).map_err(|e| e.to_string())?;
    let mut meta = serde_json::Map::new();
    meta.insert("view".into(), Value::String("stripped".into()));
    meta.insert("original_line_count".into(), Value::from(total_lines));
    meta.insert("lines_stripped".into(), Value::from(lines_stripped));
    Ok(finalize_response(None, meta, body, budget))
}

/// Returns `(body, total_lines)` so the dispatcher can build view-meta
/// without re-reading the file. `total_lines` is the file's raw line count
/// before any view shaping (caller exposes it as `original_line_count`).
fn read_signature_file(
    path: &Path,
    cache: &OutlineCache,
) -> Result<(String, u32), crate::error::TilthError> {
    let content = std::fs::read_to_string(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => crate::error::TilthError::NotFound {
            path: path.to_path_buf(),
            suggestion: None,
        },
        std::io::ErrorKind::PermissionDenied => crate::error::TilthError::PermissionDenied {
            path: path.to_path_buf(),
        },
        _ => crate::error::TilthError::IoError {
            path: path.to_path_buf(),
            source: e,
        },
    })?;
    let meta = std::fs::metadata(path).map_err(|e| crate::error::TilthError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;
    let line_count = content.lines().count() as u32;
    let header = crate::format::file_header(
        path,
        meta.len(),
        line_count,
        crate::types::ViewMode::Signature,
    );
    let crate::types::FileType::Code(lang) = crate::lang::detect_file_type(path) else {
        let body = crate::read::read_file(path, None, false, cache, false)?;
        return Ok((body, line_count));
    };
    let entries = crate::lang::outline::get_outline_entries(&content, lang);
    let lines: Vec<&str> = content.lines().collect();
    let mut body = String::new();
    render_signature_entries(&entries, &lines, &mut body);
    if body.is_empty() {
        body = crate::format::hashlines(&content, 1);
    }
    Ok((format!("{header}\n\n{}", body.trim_end()), line_count))
}

fn render_signature_entries(
    entries: &[crate::types::OutlineEntry],
    lines: &[&str],
    out: &mut String,
) {
    for entry in entries {
        let idx = entry.start_line.saturating_sub(1) as usize;
        if let Some(line) = lines.get(idx) {
            let hash = crate::format::line_hash(line.as_bytes());
            let _ = writeln!(out, "{}:{hash:03x}|{line}", entry.start_line);
        }
        render_signature_entries(&entry.children, lines, out);
    }
}

/// `mode=stripped`: whole-file read with plain comments, debug logging, and
/// repeated blank lines removed. Doc comments and TODO/FIXME-style markers
/// are preserved. Reuses the same `strip_noise` heuristic search uses when
/// expanding match bodies. Rendered with original 1-indexed line numbers in
/// a left gutter so the agent sees both surviving content and which line
/// numbers were dropped. Hashlines are intentionally suppressed even in
/// edit mode: stripped output is non-contiguous with the file on disk and
/// cannot round-trip through `tilth_write`. Non-code files fall back to
/// the default view (same escape hatch as `read_signature_file`).
///
/// Returns `(body, total_lines, lines_stripped)` so the dispatcher can
/// build view-meta without re-counting.
fn read_stripped_file(
    path: &Path,
    cache: &OutlineCache,
) -> Result<(String, u32, u32), crate::error::TilthError> {
    let content = std::fs::read_to_string(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => crate::error::TilthError::NotFound {
            path: path.to_path_buf(),
            suggestion: None,
        },
        std::io::ErrorKind::PermissionDenied => crate::error::TilthError::PermissionDenied {
            path: path.to_path_buf(),
        },
        _ => crate::error::TilthError::IoError {
            path: path.to_path_buf(),
            source: e,
        },
    })?;
    let meta = std::fs::metadata(path).map_err(|e| crate::error::TilthError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;
    let total_lines = u32::try_from(content.lines().count()).unwrap_or(u32::MAX);

    // Non-code: stripped means nothing without comment syntax. Fall through
    // to the default outline-or-full view, consistent with read_signature_file.
    if !matches!(
        crate::lang::detect_file_type(path),
        crate::types::FileType::Code(_)
    ) {
        let body = crate::read::read_file(path, None, false, cache, false)?;
        return Ok((body, total_lines, 0));
    }

    let skip_lines = crate::search::strip::strip_noise(&content, path, Some((1, total_lines)));

    let last_line = (total_lines.max(1)) as usize;
    let width = (last_line.ilog10() + 1) as usize;
    let mut body = String::with_capacity(content.len());
    let mut kept: u32 = 0;
    for (i, line) in content.lines().enumerate() {
        let line_num = u32::try_from(i + 1).unwrap_or(u32::MAX);
        if skip_lines.contains(&line_num) {
            continue;
        }
        let _ = writeln!(body, "{line_num:>width$}  {line}");
        kept += 1;
    }
    let stripped = total_lines.saturating_sub(kept);
    let header = crate::format::file_header(
        path,
        meta.len(),
        total_lines,
        crate::types::ViewMode::Stripped,
    );
    let note = format!(
        "// stripped {stripped} of {total_lines} lines (plain comments, debug logs, blank collapse) — non-editable view"
    );
    Ok((
        format!("{header}\n{note}\n\n{}", body.trim_end()),
        total_lines,
        stripped,
    ))
}
