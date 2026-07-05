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

pub(in crate::mcp) fn tool_read(
    args: &Value,
    cache: &OutlineCache,
    session: &Session,
    edit_mode: bool,
) -> Result<String, String> {
    // Default to DEFAULT_BUDGET when the caller omits `budget`, matching
    // `apply_budget` used by the other tools — an uncapped `mode=full` or
    // multi-file batch read would otherwise exceed the host response limit.
    let budget_val = args
        .get("budget")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(crate::budget::DEFAULT_BUDGET);
    let budget = Some(budget_val);

    let paths_arr = match args.get("paths") {
        Some(v) => v.as_array().ok_or(
            "paths must be an array of file paths (use single-element array for one file)",
        )?,
        None => return Err("missing required parameter: paths (array of file paths)".into()),
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

    // The absolute checkout directory anchors every relative path.
    let cwd = super::require_cwd(args)?;

    // Resolve suffix grammar on each path spec into (PathBuf, Suffix). Relative
    // paths anchor under `cwd`; absolute paths pass through (trust-absolute).
    let parsed: Vec<(PathBuf, PathSuffix)> = raw_paths
        .iter()
        .map(|s| {
            let (p, suffix) = crate::mcp::path_suffix::parse_path_with_suffix(s);
            Ok((super::resolve_anchored(&p, cwd)?, suffix))
        })
        .collect::<Result<_, String>>()?;
    let paths: Vec<PathBuf> = parsed.iter().map(|(p, _)| p.clone()).collect();
    let suffixes: Vec<&PathSuffix> = parsed.iter().map(|(_, s)| s).collect();

    let now = std::time::SystemTime::now();

    // Multi-file batch: per-file smart view applies, but no related-file hints
    // (those only make sense for whole-file reads of a single target).
    if paths.len() > 1 {
        use rayon::prelude::*;

        // Per-path outcome. Workers are pure (read file, parse outline, format)
        // except for `session.record_read` (atomic + Mutex internally) and
        // `cache` access (DashMap). Partitioned after the join to preserve
        // input order — `par_iter().collect()` is index-stable.
        enum PerPath {
            Content(String),
            NotFound(String),
        }

        let outcomes: Vec<PerPath> = parsed
            .par_iter()
            .map(|(path, suffix)| {
                if !path.exists() {
                    return PerPath::NotFound(path.display().to_string());
                }
                if crate::read::tilthignore_denies(path) {
                    return PerPath::Content(crate::read::blocked_notice(path));
                }
                // A `#symbol` suffix that resolves cleanly to "symbol absent
                // from outline" is the symbol-equivalent of a missing file:
                // route it to the `── not found ──` footer with the qualified
                // `<path>#<symbol>` form. Precondition failures (unreadable
                // file, non-code file) fall through to the existing inline
                // error path so we don't misclassify them as "not found".
                if let PathSuffix::Symbol(name) = suffix {
                    if matches!(resolve_symbol(path, name), SymbolLookup::Missing) {
                        return PerPath::NotFound(format!("{}#{}", path.display(), name));
                    }
                }
                session.record_read(path);
                if let Some(s_ts) = since {
                    if !crate::mcp::iso::file_changed_since(path, s_ts) {
                        return PerPath::Content(crate::mcp::iso::unchanged_stub(path, s_ts));
                    }
                }
                let signature = force_signature
                    || (!force_full
                        && mode_str == "auto"
                        && matches!(suffix, PathSuffix::None)
                        && should_auto_signature(path));
                let (body, spec) = if force_full && matches!(suffix, PathSuffix::None) {
                    let b = crate::read::read_file(path, None, true, cache, edit_mode)
                        .unwrap_or_else(|e| format!("# {}\nerror: {}", path.display(), e));
                    (b, crate::read::SeenSpec::Whole)
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
                // Record the whole-file-tag snapshot so a follow-up tilth_write
                // can verify the tag, using the seen-lines the view displayed.
                if edit_mode
                    && should_record_edit_snapshot(
                        path,
                        suffix,
                        signature,
                        force_stripped,
                        force_full,
                    )
                {
                    crate::read::record_edit_snapshot(session, path, &spec);
                }
                PerPath::Content(body)
            })
            .collect();

        let mut parts: Vec<String> = Vec::with_capacity(parsed.len());
        let mut not_found: Vec<String> = Vec::new();
        for outcome in outcomes {
            match outcome {
                PerPath::Content(s) => parts.push(s),
                PerPath::NotFound(s) => not_found.push(s),
            }
        }
        // Batch budget: split the total across files so every file is
        // represented (no silent trailing drops). Only the budget is split
        // here — each file's smart-view shaping above is left untouched.
        // Per-file truncation cites the total budget; finalize_response
        // applies the aggregate ceiling.
        let per_file = crate::budget::item_budget(budget_val, parts.len().max(1));
        let parts: Vec<String> = parts
            .into_iter()
            .map(|p| crate::budget::apply_item(&p, per_file, budget_val))
            .collect();
        let mut combined = parts.join("\n\n");
        if !not_found.is_empty() {
            if !combined.is_empty() {
                combined.push_str("\n\n");
            }
            combined.push_str("── not found ──");
            for p in &not_found {
                let _ = write!(combined, "\n{p}");
            }
        }
        // Multi-file responses don't carry per-file view-meta — the agent
        // can read each per-file `# path (...) [mode]` header inline.
        return Ok(finalize_response(
            Some(now),
            serde_json::Map::new(),
            &combined,
            budget,
        ));
    }

    let path = paths.into_iter().next().expect("paths non-empty");
    let suffix = suffixes
        .into_iter()
        .next()
        .cloned()
        .unwrap_or(PathSuffix::None);

    // A repo's .tilthignore hard-denies an explicit read of this path.
    if crate::read::tilthignore_denies(&path) {
        return Ok(finalize_response(
            None,
            serde_json::Map::new(),
            &crate::read::blocked_notice(&path),
            None,
        ));
    }

    // if_modified_since on a single path
    if let Some(s_ts) = since {
        if !crate::mcp::iso::file_changed_since(&path, s_ts) {
            let body = crate::mcp::iso::unchanged_stub(&path, s_ts);
            return Ok(finalize_response(
                Some(now),
                serde_json::Map::new(),
                &body,
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
        let (body, spec) =
            read_single_with_suffix(&path, &suffix, force_signature, false, edit_mode, cache);
        if edit_mode
            && should_record_edit_snapshot(&path, &suffix, force_signature, false, force_full)
        {
            crate::read::record_edit_snapshot(session, &path, &spec);
        }
        // Suffix-driven reads carry no view-meta — the LLM declared the slice.
        // Cache token still rides along when if_modified_since was supplied.
        return Ok(finalize_response(
            since.map(|_| now),
            serde_json::Map::new(),
            &body,
            budget,
        ));
    }

    session.record_read(&path);

    // Only genuine AUTO reads are credited with savings — where tilth transparently
    // returns an outline instead of the full file a naive `cat` would dump. An
    // explicit signature/stripped/full read asked for a specific view, so crediting
    // "saved vs the whole file" would overstate. Suffix reads already returned above.
    let auto_read = !force_signature && !force_stripped && !force_full;
    // Capture the file size up front, close to `read_file`'s own read. Statting
    // after the read+format pipeline would let an external append in that window
    // inflate the baseline and overstate savings; statting here means a concurrent
    // grow can only *understate* it, keeping the number a conservative lower bound.
    let savings_baseline = if auto_read {
        std::fs::metadata(&path).map(|m| m.len()).ok()
    } else {
        None
    };

    let auto_signature_promotion =
        !force_full && !force_stripped && mode_str == "auto" && should_auto_signature(&path);

    // `mode: signature` (explicit or auto-promoted) is an outline-style read.
    if force_signature || auto_signature_promotion {
        let response = respond_signature(&path, cache, auto_signature_promotion, budget)?;
        // Auto-promotion is a transparent outline of a large code file — the fork's
        // equivalent of returning less than the full file, so credit the savings.
        if let Some(file_byte_len) = savings_baseline {
            session.record_savings(
                crate::types::estimate_tokens(file_byte_len),
                crate::types::estimate_tokens(response.len() as u64),
            );
        }
        return Ok(response);
    }

    // `mode: stripped` is comment/log-stripped; explicit only (no auto path).
    // Explicit shape request → no `next_view` hint, matching `mode=signature`.
    if force_stripped {
        return respond_stripped(&path, cache, budget);
    }

    // Cold-path fuzzy resolution: the server has chdir'd to the project root, so
    // the current directory is the scope for the gitignore-aware tree walk.
    let mut output =
        crate::read::read_file_resolving(&path, None, force_full, cache, edit_mode, Path::new("."))
            .map_err(|e| e.to_string())?;
    // An outlined view emits no `[path#TAG]` and no numbered lines, so it
    // displayed nothing to anchor an edit against — recording whole-file
    // seenLines here would poison the snapshot for a later range read of the
    // same content and defeat the unseen-anchor gate. Record nothing.
    if edit_mode && should_record_edit_snapshot(&path, &suffix, false, false, force_full) {
        crate::read::record_edit_snapshot(session, &path, &crate::read::SeenSpec::Whole);
    }

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
    // Keyed off the view marker in the emitted header, not `would_outline`'s
    // prediction — the never-worse outline gate (OGATE) can return full
    // content for a file that *would* outline, and labeling that body
    // `view: "outline"` would tell the LLM to re-read for content it has.
    let actually_outlined = output
        .lines()
        .next()
        .is_some_and(|l| l.ends_with("[keys]") || l.ends_with("[outline]"));
    let mut meta = serde_json::Map::new();
    if !force_full && actually_outlined {
        meta.insert("view".into(), Value::String("outline".into()));
        if let Some(total) = count_lines(&path) {
            meta.insert("original_line_count".into(), Value::from(total));
        }
        meta.insert("next_view".into(), Value::String("full".into()));
    }

    let response = finalize_response(None, meta, &output, budget);
    // Credit savings vs the full file using the baseline captured before the read.
    if let Some(file_byte_len) = savings_baseline {
        session.record_savings(
            crate::types::estimate_tokens(file_byte_len),
            crate::types::estimate_tokens(response.len() as u64),
        );
    }
    Ok(response)
}

/// Resolve a single path+suffix to its read output plus the [`SeenSpec`] the
/// view displayed (for the whole-file-tag snapshot's seen-lines provenance).
/// Symbol and heading spans are resolved exactly once here and reused for the
/// spec, so no caller re-parses to recover the same span. `signature` and
/// `stripped` are whole-file shape modes honored only for `PathSuffix::None`
/// (any explicit suffix wins); they are mutually exclusive — `signature` takes
/// precedence if both happen to be set. The returned spec for signature/stripped
/// views is `Whole`, but those views never record (see
/// [`should_record_edit_snapshot`]).
pub(crate) fn read_single_with_suffix(
    path: &Path,
    suffix: &PathSuffix,
    signature: bool,
    stripped: bool,
    edit_mode: bool,
    cache: &OutlineCache,
) -> (String, crate::read::SeenSpec) {
    use crate::read::SeenSpec;
    let cast = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    let render_err = |e: crate::error::TilthError| format!("# {}\nerror: {}", path.display(), e);
    match suffix {
        PathSuffix::LineRange(s, e) => {
            let range = format!("{s}-{e}");
            let body = crate::read::read_ranges(path, &[range.as_str()], edit_mode)
                .unwrap_or_else(render_err);
            (body, SeenSpec::Ranges(vec![(cast(*s), cast(*e))]))
        }
        PathSuffix::FromLine(n) => {
            // Resolve total lines via metadata + count; cheap & avoids full read.
            let total = count_lines(path).map_or(*n, |t| t as usize);
            let end = total.max(*n);
            let range = format!("{n}-{end}");
            let body = crate::read::read_ranges(path, &[range.as_str()], edit_mode)
                .unwrap_or_else(render_err);
            (body, SeenSpec::Ranges(vec![(cast(*n), cast(end))]))
        }
        PathSuffix::Heading(h) => {
            // Resolve the heading span once (like the symbol path) so the read
            // and the seen-lines spec share a single resolution. Absent heading →
            // read the raw anchor to surface the error and fall back to Whole.
            if let Some((s, e)) = crate::read::resolve_heading_span(path, h) {
                let range = format!("{s}-{e}");
                let body = crate::read::read_ranges(path, &[range.as_str()], edit_mode)
                    .unwrap_or_else(render_err);
                (body, SeenSpec::Ranges(vec![(s, e)]))
            } else {
                let body = crate::read::read_ranges(path, &[h.as_str()], edit_mode)
                    .unwrap_or_else(render_err);
                (body, SeenSpec::Whole)
            }
        }
        PathSuffix::Symbol(name) => {
            // Resolve symbol via outline → range once, reused for read + spec.
            match resolve_symbol_range(path, name) {
                Some((s, e)) => {
                    let range = format!("{s}-{e}");
                    let body = crate::read::read_ranges(path, &[range.as_str()], edit_mode)
                        .unwrap_or_else(render_err);
                    (body, SeenSpec::Ranges(vec![(cast(s), cast(e))]))
                }
                None => (
                    format!(
                        "# {}\nerror: symbol '{}' not found in outline",
                        path.display(),
                        name
                    ),
                    SeenSpec::Whole,
                ),
            }
        }
        PathSuffix::None => {
            debug_assert!(
                !(signature && stripped),
                "signature and stripped are mutually exclusive view modes"
            );
            if signature {
                // Multi-file batch path: discard the line count — view-meta is
                // not emitted for multi-file responses.
                let body =
                    read_signature_file(path, cache).map_or_else(render_err, |(body, _)| body);
                return (body, SeenSpec::Whole);
            }
            if stripped {
                let body =
                    read_stripped_file(path, cache).map_or_else(render_err, |(body, _, _)| body);
                return (body, SeenSpec::Whole);
            }
            let body = crate::read::read_file(path, None, false, cache, edit_mode)
                .unwrap_or_else(render_err);
            (body, SeenSpec::Whole)
        }
    }
}

/// Whether an edit-mode read of `path` with `suffix` should record a
/// whole-file-tag snapshot (callers gate on `edit_mode` first). Returns `false`
/// for signature and stripped views (non-editable, no tag emitted), and an
/// outlined whole-file
/// view (no numbered lines shown — recording whole-file seen-lines would poison
/// the snapshot for a later range read and defeat the unseen-anchor gate). The
/// single predicate the three `tool_read` dispatch sites share.
fn should_record_edit_snapshot(
    path: &Path,
    suffix: &PathSuffix,
    signature: bool,
    stripped: bool,
    force_full: bool,
) -> bool {
    if signature || stripped {
        return false;
    }
    if matches!(suffix, PathSuffix::None) && !force_full && crate::read::would_outline(path) {
        return false;
    }
    true
}

fn find_symbol_entry(entries: &[crate::types::OutlineEntry], name: &str) -> Option<(usize, usize)> {
    crate::lang::outline::find_entry_by_name(entries, name).map(|(s, e)| (s as usize, e as usize))
}

/// Outcome of looking up `name` in `path`'s outline. Distinguishes a
/// genuine miss (file parsed cleanly, symbol absent) from precondition
/// failures (file unreadable, or not a code-with-grammar file). Lets the
/// multi-file batch route only true misses to the `── not found ──`
/// footer instead of misclassifying I/O or file-type errors as such.
enum SymbolLookup {
    Found(usize, usize),
    Missing,
    PreconditionFailed,
}

fn resolve_symbol(path: &Path, name: &str) -> SymbolLookup {
    let Ok(content) = std::fs::read_to_string(path) else {
        return SymbolLookup::PreconditionFailed;
    };
    let crate::types::FileType::Code(lang) = crate::lang::detect_file_type(path) else {
        return SymbolLookup::PreconditionFailed;
    };
    let entries = crate::lang::outline::get_outline_entries(&content, lang);
    match find_symbol_entry(&entries, name) {
        Some((s, e)) => SymbolLookup::Found(s, e),
        None => SymbolLookup::Missing,
    }
}

/// Back-compat shim for `read_single_with_suffix`, which collapses all
/// non-Found outcomes into a single inline error message.
fn resolve_symbol_range(path: &Path, name: &str) -> Option<(usize, usize)> {
    match resolve_symbol(path, name) {
        SymbolLookup::Found(s, e) => Some((s, e)),
        SymbolLookup::Missing | SymbolLookup::PreconditionFailed => None,
    }
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
    body: &str,
    budget: Option<u64>,
) -> String {
    let Some(total_budget) = budget else {
        return crate::mcp::iso::with_meta_header(now, meta, body);
    };

    let body_budget = |meta: &serde_json::Map<String, Value>| {
        let header_preview = crate::mcp::iso::with_meta_header(now, meta.clone(), "");
        let header_tokens = crate::types::estimate_tokens(header_preview.len() as u64);
        total_budget.saturating_sub(header_tokens + 16)
    };

    let (mut body_final, info) = crate::budget::apply_with_info(body, body_budget(&meta));
    if let Some(info) = info {
        meta.insert("truncated".into(), Value::Bool(true));
        meta.insert("truncated_at_line".into(), Value::from(info.at_line));
        // Don't clobber an already-set original_line_count (the caller may have
        // set a more accurate value from a non-budgeted source).
        meta.entry("original_line_count")
            .or_insert_with(|| Value::from(info.original_line_count));
        let (final_body, final_info) = crate::budget::apply_with_info(body, body_budget(&meta));
        if let Some(info) = final_info {
            meta.insert("truncated_at_line".into(), Value::from(info.at_line));
            meta.entry("original_line_count")
                .or_insert_with(|| Value::from(info.original_line_count));
        }
        body_final = final_body;
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
    Ok(finalize_response(None, meta, &body, budget))
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
    Ok(finalize_response(None, meta, &body, budget))
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
        body = crate::edit::tag::render_numbered_slice(&content, 1);
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
            let _ = writeln!(out, "{}:{line}", entry.start_line);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::OutlineCache;
    use crate::session::Session;

    fn services() -> (Session, OutlineCache) {
        (Session::new(), OutlineCache::new())
    }

    #[test]
    fn read_relative_path_anchors_under_cwd() {
        // A relative path + cwd reads from <cwd>/<path>, not <server-cwd>/<path>.
        // Prevents worktree agents from silently reading the wrong checkout.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("hello.rs"), "fn hello() {}").unwrap();

        let (session, cache) = services();
        let args = serde_json::json!({
            "paths": ["hello.rs"],
            "mode": "full",
            "cwd": root.to_str().unwrap()
        });
        let result = tool_read(&args, &cache, &session, false).unwrap();
        assert!(
            result.contains("fn hello()"),
            "expected file content via cwd-anchored path, got: {result}"
        );
    }

    #[test]
    fn read_absolute_path_ignores_cwd() {
        // Absolute paths are used as-is even when cwd points elsewhere.
        let tmp = tempfile::tempdir().unwrap();
        let abs_file = tmp.path().join("abs.rs");
        std::fs::write(&abs_file, "fn abs() {}").unwrap();

        let unrelated = tempfile::tempdir().unwrap();
        let (session, cache) = services();
        let args = serde_json::json!({
            "paths": [abs_file.to_str().unwrap()],
            "mode": "full",
            "cwd": unrelated.path().to_str().unwrap()
        });
        let result = tool_read(&args, &cache, &session, false).unwrap();
        assert!(
            result.contains("fn abs()"),
            "absolute path must resolve independently of cwd, got: {result}"
        );
    }

    #[test]
    fn read_absolute_path_outside_cwd_succeeds() {
        // Trust-absolute posture: an absolute path OUTSIDE cwd (e.g. a linked
        // worktree) is honored, not refused. cwd is still required as the anchor.
        let checkout = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let abs_file = elsewhere.path().join("check.rs");
        std::fs::write(&abs_file, "fn check() {}").unwrap();

        let (session, cache) = services();
        let args = serde_json::json!({
            "paths": [abs_file.to_str().unwrap()],
            "mode": "full",
            "cwd": checkout.path().to_str().unwrap()
        });
        let result = tool_read(&args, &cache, &session, false).unwrap();
        assert!(
            result.contains("fn check()"),
            "absolute path outside cwd must read (trust-absolute), got: {result}"
        );
    }

    #[test]
    fn read_missing_cwd_refused_with_teaching_error() {
        // A read without cwd is refused before any path resolution — the server
        // cannot see the caller's shell cwd, so it must be told the checkout dir.
        let (session, cache) = services();
        let args = serde_json::json!({ "paths": ["src/foo.rs"], "mode": "full" });
        let err = tool_read(&args, &cache, &session, false).unwrap_err();
        assert!(
            err.contains("cwd") && err.contains("absolute checkout directory"),
            "missing cwd must refuse with the teaching error: {err}"
        );
    }

    #[test]
    fn read_relative_dotdot_path_refused() {
        // A relative `..` path must not climb out of cwd.
        let tmp = tempfile::tempdir().unwrap();
        let (session, cache) = services();
        let args = serde_json::json!({
            "paths": ["../escape.rs"],
            "mode": "full",
            "cwd": tmp.path().to_str().unwrap()
        });
        let err = tool_read(&args, &cache, &session, false).unwrap_err();
        assert!(
            err.contains("escapes") && err.contains(".."),
            "relative `..` path must be refused: {err}"
        );
    }

    /// A large non-code file reads as an OUTLINE in `mode:auto` — no
    /// `[path#TAG]` and no numbered `N:content` rows are displayed. Recording
    /// whole-file seenLines here poisons the snapshot so a later range read of
    /// the same content inherits an all-lines seen set, defeating the
    /// unseen-anchor gate. The outline read must record nothing.
    #[test]
    fn single_path_auto_outline_read_grants_no_whole_file_seen_lines() {
        use crate::index::bloom::BloomFilterCache;
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let p = root.join("big.md");
        let mut content = String::new();
        // Few headings, large plain-text bodies: the markdown outline (headings
        // only) is a small fraction of the full-file token cost, so OGATE does
        // not fire and the read genuinely returns an outline (not full content).
        for i in 1..=40 {
            let _ = writeln!(content, "# Section {i}");
            for j in 0..60 {
                let _ = writeln!(
                    content,
                    "Body paragraph line {j} of section {i} with padding text to bloat bytes."
                );
            }
        }
        std::fs::write(&p, &content).unwrap();
        assert!(
            crate::read::would_outline(&p),
            "fixture must be large enough to outline"
        );

        let (session, cache) = services();
        let cwd = root.to_str().unwrap();

        // Edit-mode auto read → outline (the path under test).
        tool_read(
            &serde_json::json!({"paths": [p.to_str().unwrap()], "cwd": cwd}),
            &cache,
            &session,
            true,
        )
        .expect("edit-mode auto read");

        // Range read of the same content records only lines 1-3.
        let range = format!("{}#1-3", p.to_str().unwrap());
        tool_read(
            &serde_json::json!({"paths": [range], "cwd": cwd}),
            &cache,
            &session,
            true,
        )
        .expect("edit-mode range read");

        // An edit anchored on line 50 (never displayed) must be rejected.
        let tag = crate::edit::tag::format_tag(crate::edit::tag::compute_file_hash(&content));
        let edits = serde_json::json!([{ "path": p.to_str().unwrap(), "tag": tag, "ops": [{ "op": "replace", "start": 50, "end": 50, "content": "# edited line 50" }] }]);
        let bloom = Arc::new(BloomFilterCache::new());
        let out = crate::mcp::tools::tool_write(
            &serde_json::json!({"edits": edits, "cwd": cwd}),
            &session,
            &bloom,
        )
        .expect("write call");
        assert!(
            out.contains("never displayed"),
            "outline read must not grant whole-file seenLines; line-50 edit must be rejected, got:\n{out}"
        );
        assert!(
            !out.contains("applied"),
            "edit on an unseen line must not apply, got:\n{out}"
        );
    }

    /// A markdown `#heading` edit-mode read records only the heading's section
    /// as seen (not the whole file): a tag-matched edit inside that section
    /// applies, but one anchored on a line in a different, never-displayed
    /// section is rejected by the seen-lines gate.
    #[test]
    fn heading_read_gates_edit_to_displayed_section() {
        use crate::index::bloom::BloomFilterCache;
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let p = root.join("doc.md");
        let content = "# Section A\nalpha\nmore a\n\n# Section B\nbeta\nmore b\n";
        std::fs::write(&p, content).unwrap();
        let (session, cache) = services();
        let cwd = root.to_str().unwrap();

        // Heading read displays only Section A (lines 1-4).
        let out = tool_read(
            &serde_json::json!({"paths": [format!("{}#{}", p.display(), "# Section A")], "cwd": cwd}),
            &cache,
            &session,
            true,
        )
        .expect("heading read");
        assert!(
            out.contains("alpha"),
            "heading read must display Section A, got:\n{out}"
        );

        let tag = crate::edit::tag::format_tag(crate::edit::tag::compute_file_hash(content));
        let bloom = Arc::new(BloomFilterCache::new());

        // Edit on line 6 (inside Section B, never displayed) is rejected.
        let reject = serde_json::json!([{ "path": p.to_str().unwrap(), "tag": tag, "ops": [{ "op": "replace", "start": 6, "end": 6, "content": "BETA" }] }]);
        let out = crate::mcp::tools::tool_write(
            &serde_json::json!({"edits": reject, "cwd": cwd}),
            &session,
            &bloom,
        )
        .expect("write call");
        assert!(
            out.contains("never displayed"),
            "heading read must not grant whole-file seenLines; a Section-B edit must be rejected, got:\n{out}"
        );
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            content,
            "rejected edit must not touch the file"
        );

        // Edit on line 2 (inside the displayed Section A) applies.
        let ok = serde_json::json!([{ "path": p.to_str().unwrap(), "tag": tag, "ops": [{ "op": "replace", "start": 2, "end": 2, "content": "ALPHA" }] }]);
        let out = crate::mcp::tools::tool_write(
            &serde_json::json!({"edits": ok, "cwd": cwd}),
            &session,
            &bloom,
        )
        .expect("write call");
        assert!(
            out.contains("applied"),
            "in-section edit must apply, got:\n{out}"
        );
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "# Section A\nALPHA\nmore a\n\n# Section B\nbeta\nmore b\n"
        );
    }

    /// Same defect on the multi-path branch: an outlined file in a batch read
    /// must not record whole-file seenLines either.
    #[test]
    fn multi_path_auto_outline_read_grants_no_whole_file_seen_lines() {
        use crate::index::bloom::BloomFilterCache;
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let big = root.join("big.md");
        let small = root.join("small.md");
        let mut content = String::new();
        for i in 1..=2000 {
            let _ = writeln!(
                content,
                "# Section {i} with enough padding text to bloat bytes"
            );
        }
        std::fs::write(&big, &content).unwrap();
        std::fs::write(&small, "# small\n").unwrap();
        assert!(crate::read::would_outline(&big), "fixture must outline");

        let (session, cache) = services();
        let cwd = root.to_str().unwrap();

        // Multi-path edit-mode auto read → big.md outlines.
        tool_read(
            &serde_json::json!({"paths": [big.to_str().unwrap(), small.to_str().unwrap()], "cwd": cwd}),
            &cache,
            &session,
            true,
        )
        .expect("edit-mode multi read");

        let range = format!("{}#1-3", big.to_str().unwrap());
        tool_read(
            &serde_json::json!({"paths": [range], "cwd": cwd}),
            &cache,
            &session,
            true,
        )
        .expect("edit-mode range read");

        let tag = crate::edit::tag::format_tag(crate::edit::tag::compute_file_hash(&content));
        let edits = serde_json::json!([{ "path": big.to_str().unwrap(), "tag": tag, "ops": [{ "op": "replace", "start": 50, "end": 50, "content": "# edited line 50" }] }]);
        let bloom = Arc::new(BloomFilterCache::new());
        let out = crate::mcp::tools::tool_write(
            &serde_json::json!({"edits": edits, "cwd": cwd}),
            &session,
            &bloom,
        )
        .expect("write call");
        assert!(
            out.contains("never displayed"),
            "multi-path outline read must not grant whole-file seenLines, got:\n{out}"
        );
    }

    // -- savings recording tests ------------------------------------------

    /// A large file with large function bodies read in auto mode (outline) must record
    /// saved > 0 and baseline > 0 on the session.
    #[test]
    fn tool_read_large_file_records_positive_savings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.rs");
        // Build a file large enough to exceed TOKEN_THRESHOLD (6 000 tokens ≈ 24 KB)
        // with functions that have substantial bodies so the outline compresses well.
        let mut src = String::from("// header\n");
        for i in 0..200 {
            let _ = writeln!(src, "fn func_{i}() {{");
            // 20 lines of body per function so outline is much smaller than full content
            for j in 0..20 {
                let _ = writeln!(src, "    let v_{i}_{j}: u64 = {j} * {i} + 42;");
            }
            src.push_str("}\n");
        }
        std::fs::write(&path, &src).unwrap();
        let file_size = std::fs::metadata(&path).unwrap().len();
        assert!(
            file_size > 24_000,
            "test file must be large enough to trigger outline: {file_size} bytes"
        );
        let cache = OutlineCache::new();
        let session = Session::new();
        let args = serde_json::json!({ "paths": [path.to_str().unwrap()], "cwd": dir.path().to_str().unwrap() });

        tool_read(&args, &cache, &session, false).expect("large file read");

        let (baseline, saved) = session.savings();
        assert!(
            baseline > 0,
            "baseline must be > 0 for a non-empty file: baseline={baseline}"
        );
        assert!(
            saved > 0,
            "large outlined file must record positive savings: saved={saved}, baseline={baseline}"
        );
    }

    /// A small file read in auto mode (full content) must record baseline > 0
    /// but saved == 0 (no reduction applied).
    #[test]
    fn tool_read_small_file_records_zero_savings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.rs");
        std::fs::write(&path, "fn small() {}\n").unwrap();
        let cache = OutlineCache::new();
        let session = Session::new();
        let args = serde_json::json!({ "paths": [path.to_str().unwrap()], "cwd": dir.path().to_str().unwrap() });

        tool_read(&args, &cache, &session, false).expect("small file read");

        let (baseline, saved) = session.savings();
        assert!(baseline > 0, "baseline must be > 0 for a non-empty file");
        assert_eq!(
            saved, 0,
            "small file returned in full must record zero savings"
        );
    }

    /// A single-section read requested an explicit range — the naive baseline is
    /// that range, not the whole file — so it must NOT record a (bogus) full-file
    /// saving. Guards against over-counting explicit sub-view reads.
    #[test]
    fn tool_read_section_records_no_savings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sectioned.rs");
        // Large file: a full-file baseline would book a big (bogus) "saving".
        let mut src = String::new();
        for i in 0..500 {
            let _ = writeln!(src, "fn f_{i}() {{ let v = {i}; }}");
        }
        std::fs::write(&path, &src).unwrap();
        let cache = OutlineCache::new();
        let session = Session::new();
        let args = serde_json::json!({ "paths": [format!("{}#1-5", path.display())], "cwd": dir.path().to_str().unwrap() });

        tool_read(&args, &cache, &session, false).expect("section read");

        let (baseline, saved) = session.savings();
        assert_eq!(
            baseline, 0,
            "section reads must not record a full-file baseline"
        );
        assert_eq!(saved, 0, "section reads must not record savings");
    }
}
