use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::session::Session;
use crate::types::ViewMode;
use tilth_core::cache::OutlineCache;
use tilth_core::error::TilthError;
use tilth_core::lang::detect_file_type;
use tilth_core::lang::outline::get_outline_entries;
use tilth_core::types::{FileType, OutlineEntry, OutlineKind};

use super::apply_budget;

pub(in crate::mcp) fn tool_read(
    args: &Value,
    cache: &OutlineCache,
    session: &Session,
    edit_mode: bool,
) -> Result<String, String> {
    let budget = args.get("budget").and_then(serde_json::Value::as_u64);
    // The budget every read is fitted to. With no `budget` argument it is
    // the default cap the response gets anyway, so the outline is fitted
    // to it instead of being cut there mid-entry.
    let read_budget = budget.unwrap_or(crate::budget::DEFAULT_BUDGET);
    // Extract optional root for anchoring relative paths. A relative path
    // without an absolute `root` is unresolvable (the server cannot see the
    // caller's shell cwd) — propagate that refusal at each path-resolution site.
    let root = args
        .get("root")
        .and_then(|v| v.as_str())
        .map(std::path::Path::new);
    let full_flag = args
        .get("full")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let mode_str = args.get("mode").and_then(|v| v.as_str()).unwrap_or("auto");
    if !matches!(mode_str, "auto" | "full" | "signature" | "stripped") {
        return Err(format!(
            "unknown read mode: {mode_str}. Use: auto, full, signature, stripped"
        ));
    }
    // Precedence when full:true is combined with a reshaping mode: signature and
    // stripped win over full. The dispatch below checks force_signature/force_stripped
    // before falling back to force_full, so `full:true` + `mode:signature` yields a
    // signature view, not a full dump. Pinned by tool_read_signature_beats_full_flag.
    let force_full = full_flag || mode_str == "full";
    let force_signature = mode_str == "signature";
    let force_stripped = mode_str == "stripped";

    // Multi-file batch read (capped at 20 to bound I/O)
    if let Some(paths_arr) = args.get("paths").and_then(|v| v.as_array()) {
        if paths_arr.len() > 20 {
            return Err(format!(
                "batch read limited to 20 files (got {})",
                paths_arr.len()
            ));
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
        // Each file gets an equal share of what the files before it left,
        // so a big first file cannot use up the budget of the rest.
        let mut used = 0u64;
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
            let path = super::resolve_read_path(&PathBuf::from(path_str), root)?;
            session.record_read(&path);
            let read = if force_signature {
                read_signature_file(&path, cache).map(|(body, _)| body)
            } else if force_stripped {
                read_stripped_file(&path, cache).map(|(body, _, _)| body)
            } else {
                let left = (paths_arr.len() - i) as u64;
                let share = read_budget.saturating_sub(used) / left;
                crate::read::read_file(&path, None, force_full, cache, edit_mode, Some(share))
            };
            let output = match read {
                Ok(output) => output,
                Err(e) => format!("# {} — error: {}", path.display(), e),
            };
            used += crate::types::estimate_tokens(output.len() as u64 + 2);
            results.push(output);
        }
        let combined = results.join("\n\n");
        return Ok(apply_budget(&combined, budget));
    }

    // Single file read
    let path_str = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("missing required parameter: path (or use paths for batch read)")?;
    let path = super::resolve_read_path(&PathBuf::from(path_str), root)?;
    let section = args.get("section").and_then(|v| v.as_str());
    let sections_arr = args.get("sections").and_then(|v| v.as_array());

    if section.is_some() && sections_arr.is_some() {
        return Err("provide either section (single) or sections (array), not both".into());
    }

    // signature/stripped reshape the whole file; a section selection has no
    // meaning there. Error rather than silently dropping the mode.
    if (force_signature || force_stripped) && (section.is_some() || sections_arr.is_some()) {
        return Err(format!(
            "mode={mode_str} cannot be combined with section/sections — \
             {mode_str} reshapes the whole file. Drop section/sections or pick mode=auto/full."
        ));
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
        return crate::read::read_ranges_with_budget(&path, &ranges, edit_mode, read_budget)
            .map_err(|e| e.to_string());
    }

    session.record_read(&path);

    // The related-file hint for outlined code files (not section reads, not
    // batch). It is built first, so the read is fitted to the room it leaves
    // and the final cut never removes the note that says what a fit left out.
    let mut related_hint = String::new();
    if section.is_none() && crate::read::would_outline(&path) {
        let related = tilth_core::imports::resolve_related_files(&path, cache);
        if !related.is_empty() {
            related_hint.push_str("\n\n> Related: ");
            for (i, p) in related
                .iter()
                .take(crate::read::RELATED_HINT_LIMIT)
                .enumerate()
            {
                if i > 0 {
                    related_hint.push_str(", ");
                }
                let _ = write!(related_hint, "{}", p.display());
            }
        }
    }
    let file_budget =
        read_budget.saturating_sub(crate::types::estimate_tokens(related_hint.len() as u64));

    let mut output = if section.is_none() && force_signature {
        read_signature_file(&path, cache)
            .map(|(body, _)| body)
            .map_err(|e| e.to_string())?
    } else if section.is_none() && force_stripped {
        read_stripped_file(&path, cache)
            .map(|(body, _, _)| body)
            .map_err(|e| e.to_string())?
    } else {
        crate::read::read_file(&path, section, force_full, cache, edit_mode, Some(file_budget))
            .map_err(|e| e.to_string())?
    };
    output.push_str(&related_hint);

    Ok(apply_budget(&output, budget))
}

// `cache` is intentionally unwired on the tree-sitter path: OutlineCache stores
// formatted outline strings, not Vec<OutlineEntry>, so get_outline_entries below
// re-parses every call. Wiring a structured cache is a separate change. The param
// is still used by the non-code fallback (read_file), so it keeps its real name.
fn read_signature_file(path: &Path, cache: &OutlineCache) -> Result<(String, u32), TilthError> {
    let content = std::fs::read_to_string(path).map_err(|e| match e.kind() {
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
    let meta = std::fs::metadata(path).map_err(|e| TilthError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;
    let line_count = u32::try_from(content.lines().count()).unwrap_or(u32::MAX);

    let FileType::Code(lang) = tilth_core::lang::detect_file_type_with_content(path, &content)
    else {
        let body = crate::read::read_file(path, None, false, cache, false, None)?;
        return Ok((body, line_count));
    };

    // Build the signature header only on the code path — the non-code fallback
    // above returns the normal read and never uses it.
    let header = crate::format::file_header(path, meta.len(), line_count, ViewMode::Signature);
    // Parse without the byte-order mark, so a line-1 import is still one,
    // but print the file's own lines: their hash anchors must match what
    // `tilth_write` reads from disk. Removing the mark keeps line numbers.
    let entries = get_outline_entries(tilth_core::source::strip_bom(&content), lang);
    let lines: Vec<&str> = content.lines().collect();
    let mut body = String::new();
    render_signature_entries(&entries, &lines, &mut body);
    if body.is_empty() {
        body = crate::format::hashlines(&content, 1);
    }
    Ok((format!("{header}\n\n{}", body.trim_end()), line_count))
}

fn render_signature_entries(entries: &[OutlineEntry], lines: &[&str], out: &mut String) {
    let mut previous_start = None;
    for entry in entries {
        // The specifiers of one export clause are entries on the same line;
        // the line prints once.
        if previous_start == Some(entry.start_line) && entry.kind == OutlineKind::Export {
            continue;
        }
        previous_start = Some(entry.start_line);
        let idx = entry.start_line.saturating_sub(1) as usize;
        if let Some(line) = lines.get(idx) {
            let hash = crate::format::line_hash(line.as_bytes());
            let _ = writeln!(out, "{}:{hash:03x}|{line}", entry.start_line);
        }
        render_signature_entries(&entry.children, lines, out);
    }
}

// `cache` is intentionally unwired on the tree-sitter path: OutlineCache stores
// formatted outline strings, not Vec<OutlineEntry>, so strip_noise re-parses every
// call. Wiring a structured cache is a separate change. The param is still used by
// the non-code fallback (read_file), so it keeps its real name.
fn read_stripped_file(path: &Path, cache: &OutlineCache) -> Result<(String, u32, u32), TilthError> {
    // A non-editable view, so it reads like search does: without the mark.
    let content = tilth_core::source::read_source(path).map_err(|e| match e.kind() {
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
    let meta = std::fs::metadata(path).map_err(|e| TilthError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;
    let total_lines = u32::try_from(content.lines().count()).unwrap_or(u32::MAX);

    if !matches!(detect_file_type(path), FileType::Code(_)) {
        let body = crate::read::read_file(path, None, false, cache, false, None)?;
        return Ok((body, total_lines, 0));
    }

    let skip_lines = crate::search::strip::strip_noise(&content, path, Some((1, total_lines)));
    let width = total_lines.max(1).to_string().len();
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
    let header = crate::format::file_header(path, meta.len(), total_lines, ViewMode::Stripped);
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
    use crate::mcp::tools::tool_definitions;

    #[test]
    fn tool_read_signature_mode_emits_hash_prefixed_signatures() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signature.rs");
        std::fs::write(
            &path,
            "fn signature_target() {\n    let body_marker = 42;\n}\n",
        )
        .unwrap();
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "mode": "signature",
        });
        let cache = OutlineCache::new();
        let session = Session::new();

        let out = tool_read(&args, &cache, &session, false).expect("signature read");

        assert!(
            out.contains("[signature]"),
            "signature header missing: {out}"
        );
        assert!(
            out.lines()
                .any(|l| l.starts_with("1:") && l.contains("fn signature_target")),
            "hash-prefixed signature line missing: {out}"
        );
        assert!(
            !out.contains("body_marker"),
            "signature mode should omit function body: {out}"
        );
    }

    /// An export clause is one outline entry per specifier; its line prints
    /// once in signature mode.
    #[test]
    fn tool_read_signature_mode_prints_an_export_clause_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("barrel.ts");
        std::fs::write(&path, "export { a, b as c } from \"./y\";\n").unwrap();
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "mode": "signature",
        });
        let out =
            tool_read(&args, &OutlineCache::new(), &Session::new(), false).expect("signature read");
        assert_eq!(
            out.lines().filter(|l| l.starts_with("1:")).count(),
            1,
            "{out}"
        );
    }

    #[test]
    fn tool_read_stripped_mode_drops_comments_and_keeps_doc_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stripped.rs");
        std::fs::write(
            &path,
            "/// keep docs\nfn keep() {\n    // drop plain comment\n    dbg!(1);\n    println!(\"keep\");\n}\n",
        )
        .unwrap();
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "mode": "stripped",
        });
        let cache = OutlineCache::new();
        let session = Session::new();

        let out = tool_read(&args, &cache, &session, true).expect("stripped read");

        assert!(out.contains("[stripped]"), "stripped header missing: {out}");
        assert!(out.contains("/// keep docs"), "doc comment missing: {out}");
        assert!(out.contains("println!"), "kept code missing: {out}");
        assert!(
            !out.contains("drop plain comment"),
            "plain comment should be stripped: {out}"
        );
        assert!(!out.contains("dbg!"), "debug log should be stripped: {out}");
        assert!(
            !out.lines().any(|l| l.contains(':') && l.contains('|')),
            "stripped output must not expose hash anchors: {out}"
        );
    }

    #[test]
    fn tool_read_unknown_mode_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("any.rs");
        std::fs::write(&path, "fn f() {}\n").unwrap();
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "mode": "outline",
        });
        let cache = OutlineCache::new();
        let session = Session::new();

        let err = tool_read(&args, &cache, &session, false).expect_err("unknown mode must error");
        assert!(
            err.starts_with("unknown read mode: outline"),
            "error must name the bad mode: {err}"
        );
        assert!(
            err.contains("auto, full, signature, stripped"),
            "error must list valid modes: {err}"
        );
    }

    #[test]
    fn tool_read_signature_mode_non_code_falls_back_to_normal_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "alpha line\nbeta line\ngamma line\n").unwrap();
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "mode": "signature",
        });
        let cache = OutlineCache::new();
        let session = Session::new();

        let out = tool_read(&args, &cache, &session, false).expect("signature read on text");

        // Non-code falls back to the normal read: no signature header, full content.
        assert!(
            !out.contains("[signature]"),
            "non-code must not emit signature header: {out}"
        );
        assert!(out.contains("alpha line"), "content must survive: {out}");
        assert!(out.contains("gamma line"), "content must survive: {out}");
    }

    #[test]
    fn tool_read_stripped_mode_non_code_falls_back_to_normal_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "alpha line\nbeta line\ngamma line\n").unwrap();
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "mode": "stripped",
        });
        let cache = OutlineCache::new();
        let session = Session::new();

        let out = tool_read(&args, &cache, &session, false).expect("stripped read on text");

        assert!(
            !out.contains("[stripped]"),
            "non-code must not emit stripped header: {out}"
        );
        assert!(out.contains("alpha line"), "content must survive: {out}");
        assert!(out.contains("gamma line"), "content must survive: {out}");
    }

    #[test]
    fn tool_read_full_flag_is_legacy_alias_for_mode_full() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("aliased.rs");
        // Body must exceed TOKEN_THRESHOLD (6k tokens ≈ 24KB) AND compress well
        // so `auto` returns an outline rather than full content — making the
        // alias equivalence observable, not a trivial small-file match where
        // auto and full coincide. Functions have large bodies so the outline
        // (signatures only) is a small fraction of the full-file token cost,
        // ensuring OGATE does not fire and auto != full.
        let mut src = String::from("// header comment\n");
        for i in 0..80 {
            let _ = writeln!(src, "fn f_{i}() {{");
            // Large body: many statements so the outline compresses well
            for j in 0..30 {
                let _ = writeln!(src, "    let local_var_{j}_in_fn_{i}: u64 = {j} + {i};");
            }
            src.push_str("}\n");
        }
        std::fs::write(&path, &src).unwrap();
        let cache = OutlineCache::new();
        let session = Session::new();

        let via_flag = tool_read(
            &serde_json::json!({ "path": path.to_str().unwrap(), "full": true }),
            &cache,
            &session,
            false,
        )
        .expect("full:true read");
        let via_mode = tool_read(
            &serde_json::json!({ "path": path.to_str().unwrap(), "mode": "full" }),
            &cache,
            &session,
            false,
        )
        .expect("mode:full read");
        let via_auto = tool_read(
            &serde_json::json!({ "path": path.to_str().unwrap() }),
            &cache,
            &session,
            false,
        )
        .expect("auto read");

        assert_eq!(
            via_flag, via_mode,
            "full:true must be a byte-identical alias for mode='full'"
        );
        assert!(
            via_flag.contains("[full]"),
            "alias must force full view: {}",
            &via_flag[..via_flag.len().min(80)]
        );
        assert_ne!(
            via_auto, via_flag,
            "auto must outline a large file, differing from forced full"
        );
    }

    #[test]
    fn tool_read_signature_beats_full_flag() {
        // full:true + mode:signature must resolve to a signature view, not a full
        // dump. If a future change flips the dispatch order this test fails loudly.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("precedence.rs");
        std::fs::write(
            &path,
            "fn precedence_target() {\n    let body_marker = 99;\n}\n",
        )
        .unwrap();
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "mode": "signature",
            "full": true,
        });
        let cache = OutlineCache::new();
        let session = Session::new();

        let out = tool_read(&args, &cache, &session, false).expect("signature+full read");

        assert!(
            out.contains("[signature]"),
            "signature must win over full:true (header): {out}"
        );
        assert!(
            !out.contains("body_marker"),
            "signature must win over full:true (body omitted): {out}"
        );
    }

    #[test]
    fn tool_read_signature_mode_rejects_section() {
        // Combining a reshaping mode with section must error, not silently drop the
        // mode (which would return a section slice and ignore signature entirely).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conflict.rs");
        std::fs::write(&path, "fn a() {}\nfn b() {}\n").unwrap();
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "mode": "signature",
            "section": "1-1",
        });
        let cache = OutlineCache::new();
        let session = Session::new();

        let err =
            tool_read(&args, &cache, &session, false).expect_err("signature + section must error");
        assert!(
            err.contains("signature") && err.contains("section"),
            "error must name the conflict: {err}"
        );
    }

    #[test]
    fn tilth_read_schema_lists_stripped_mode() {
        let tools = tool_definitions(false);
        let read = tools
            .iter()
            .find(|tool| tool.get("name").and_then(Value::as_str) == Some("tilth_read"))
            .expect("tilth_read definition");
        let modes = read
            .pointer("/inputSchema/properties/mode/enum")
            .and_then(Value::as_array)
            .expect("mode enum");

        assert!(
            modes.iter().any(|v| v.as_str() == Some("stripped")),
            "mode enum must advertise stripped: {read}"
        );
    }

    #[test]
    fn root_param_anchors_relative_path_under_root() {
        // Guards #78: tilth_read with a relative path + root must read from
        // <root>/<path>, not from <cwd>/<path>. Prevents worktree agents from
        // silently reading the wrong checkout.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("hello.rs"), "fn hello() {}").unwrap();

        let cache = OutlineCache::new();
        let session = Session::new();
        let args = serde_json::json!({
            "paths": ["hello.rs"],
            "mode": "full",
            "root": root.to_str().unwrap()
        });
        let result = tool_read(&args, &cache, &session, false).unwrap();
        assert!(
            result.contains("fn hello()"),
            "expected file content via root-anchored path, got: {result}"
        );
    }

    #[test]
    fn root_param_absolute_path_unaffected() {
        // Absolute paths must be used as-is even when root is set.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let abs_file = root.join("abs.rs");
        std::fs::write(&abs_file, "fn abs() {}").unwrap();

        let unrelated_root = tempfile::tempdir().unwrap();
        let cache = OutlineCache::new();
        let session = Session::new();
        let args = serde_json::json!({
            "paths": [abs_file.to_str().unwrap()],
            "mode": "full",
            "root": unrelated_root.path().to_str().unwrap()
        });
        let result = tool_read(&args, &cache, &session, false).unwrap();
        assert!(
            result.contains("fn abs()"),
            "absolute path must resolve independently of root, got: {result}"
        );
    }

    #[test]
    fn no_root_reads_absolute_path_unchanged() {
        // Omitting root must behave identically to before #78: absolute paths
        // resolve as-is regardless of whether root is set or not.
        let tmp = tempfile::tempdir().unwrap();
        let abs_file = tmp.path().join("check.rs");
        std::fs::write(&abs_file, "fn check() {}").unwrap();

        let cache = OutlineCache::new();
        let session = Session::new();
        let args = serde_json::json!({
            "paths": [abs_file.to_str().unwrap()],
            "mode": "full"
        });
        let result = tool_read(&args, &cache, &session, false).unwrap();
        assert!(
            result.contains("fn check()"),
            "no-root regression: absolute path must be readable without root, got: {result}"
        );
    }

    #[test]
    fn relative_path_no_root_errors() {
        // WHY: a relative path + no root silently resolved against the frozen
        // server cwd before this spec — the worktree bug. It must now refuse
        // with a message naming the path and the absolute-root escape hatch.
        let cache = OutlineCache::new();
        let session = Session::new();
        let args = serde_json::json!({ "paths": ["src/foo.rs"], "mode": "full" });
        let err = tool_read(&args, &cache, &session, false).unwrap_err();
        assert!(
            err.contains("src/foo.rs") && err.contains("root"),
            "relative path without root must refuse with an actionable message: {err}"
        );
    }

    /// A JS file with one function per line: its outline is nearly as long
    /// as the file, and both are far over the default budget.
    fn write_dense_js(dir: &Path, name: &str) -> PathBuf {
        let mut src = String::from("import { helper } from './helper';\n");
        for i in 0..6000 {
            let _ = writeln!(
                src,
                "export function functionNumber{i:05}(alpha, beta) {{ return alpha + beta; }}"
            );
        }
        let path = dir.join(name);
        std::fs::write(&path, src).unwrap();
        std::fs::write(dir.join("helper.js"), "export function helper() {}\n").unwrap();
        path
    }

    fn read_args(args: &Value) -> String {
        tool_read(args, &OutlineCache::new(), &Session::new(), false).unwrap()
    }

    /// With no `budget`, the read skipped the outline fit: the full view
    /// came back and the default cap cut it after the first functions,
    /// hiding the rest. It reads like an explicit default budget now.
    #[test]
    fn tool_read_without_a_budget_fits_the_outline_to_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_dense_js(dir.path(), "dense.js");
        let path = path.to_str().unwrap();
        let implicit = read_args(&serde_json::json!({ "path": path }));
        let explicit = read_args(&serde_json::json!({
            "path": path,
            "budget": crate::budget::DEFAULT_BUDGET,
        }));
        assert_eq!(implicit, explicit);
        assert!(implicit.contains("[outline]"), "{}", &implicit[..200]);
    }

    /// The `> Related:` line went on after the outline was fitted, so the
    /// final cut removed the note that says what the fit left out.
    #[test]
    fn tool_read_keeps_the_fit_note_with_the_related_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_dense_js(dir.path(), "dense.js");
        let out = read_args(&serde_json::json!({
            "path": path.to_str().unwrap(),
            "budget": 24_000,
        }));
        assert!(out.contains("> outline cut to fit the budget"), "{}", tail(&out));
        assert!(out.contains("> Related: "), "{}", tail(&out));
        assert!(!out.contains("... truncated"), "{}", tail(&out));
    }

    /// A batch gave each file the whole budget, so the first big file used
    /// it up and the next was cut off behind "truncated".
    #[test]
    fn tool_read_batch_shares_the_budget_between_files() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_dense_js(dir.path(), "a.js");
        let b = write_dense_js(dir.path(), "b.js");
        let out = read_args(&serde_json::json!({
            "paths": [a.to_str().unwrap(), b.to_str().unwrap()],
        }));
        assert!(out.contains("b.js"), "{}", tail(&out));
        assert_eq!(out.matches("> outline cut to fit the budget").count(), 2, "{}", tail(&out));
        assert!(!out.contains("... truncated"), "{}", tail(&out));
    }

    /// `sections` with no budget had no cap at all, so two big ranges
    /// went past the host's response limit. They share the default now.
    #[test]
    fn tool_read_sections_without_a_budget_stay_under_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_dense_js(dir.path(), "dense.js");
        let out = read_args(&serde_json::json!({
            "path": path.to_str().unwrap(),
            "sections": ["1-3000", "3001-6000"],
        }));
        let tokens = crate::types::estimate_tokens(out.len() as u64);
        assert!(tokens <= crate::budget::DEFAULT_BUDGET, "{tokens} tokens");
    }

    fn tail(out: &str) -> &str {
        &out[out.floor_char_boundary(out.len().saturating_sub(400))..]
    }
}
