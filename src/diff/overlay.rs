use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use crate::lang::detect_file_type;
use crate::lang::outline::get_outline_entries;
use crate::types::{FileType, OutlineEntry, OutlineKind};

use super::matching::{build_diff_symbols, match_symbols};
use super::{
    ChangeType, Conflict, DiffLine, DiffLineKind, DiffSource, FileDiff, FileOverlay, FileStatus,
    MatchConfidence, SymbolChange,
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build a structural overlay for a single file diff.
///
/// Fetches old/new content based on `source`, outlines both versions,
/// runs three-phase symbol matching, and attributes diff hunks to functions.
pub(crate) fn compute_overlay(file_diff: &FileDiff, source: &DiffSource) -> FileOverlay {
    let path = &file_diff.path;

    // Binary or generated files — empty overlay, formatter handles display.
    if file_diff.is_binary || file_diff.is_generated {
        return FileOverlay {
            path: path.clone(),
            symbol_changes: Vec::new(),
            attributed_hunks: Vec::new(),
            conflicts: Vec::new(),
            new_content: None,
        };
    }

    match file_diff.status {
        FileStatus::Modified => compute_modified(file_diff, source),
        FileStatus::Added => compute_added(file_diff, source),
        FileStatus::Deleted => compute_deleted(file_diff, source),
        FileStatus::Renamed => compute_renamed(file_diff, source),
    }
}

/// Cross-file move detection: match Deleted symbols in one file with Added
/// symbols in another by (kind, name). Unique pairs become `Moved{old_path}`.
pub(crate) fn cross_file_matching(overlays: &mut [FileOverlay]) {
    // Collect all Deleted and Added symbols with their overlay index + change index.
    let mut deleted: HashMap<(OutlineKind, String), Vec<(usize, usize)>> = HashMap::new();
    let mut added: HashMap<(OutlineKind, String), Vec<(usize, usize)>> = HashMap::new();

    for (oi, overlay) in overlays.iter().enumerate() {
        for (ci, change) in overlay.symbol_changes.iter().enumerate() {
            match &change.change {
                ChangeType::Deleted => {
                    deleted
                        .entry((change.kind, change.name.clone()))
                        .or_default()
                        .push((oi, ci));
                }
                ChangeType::Added => {
                    added
                        .entry((change.kind, change.name.clone()))
                        .or_default()
                        .push((oi, ci));
                }
                _ => {}
            }
        }
    }

    // Collect mutations: (overlay_idx, change_idx, new_change_type, new_confidence)
    let mut mutations: Vec<(usize, usize, ChangeType, MatchConfidence)> = Vec::new();

    for (key, del_locs) in &deleted {
        if let Some(add_locs) = added.get(key) {
            if del_locs.len() == 1 && add_locs.len() == 1 {
                let (del_oi, del_ci) = del_locs[0];
                let (add_oi, add_ci) = add_locs[0];
                // Must be in different files to be a cross-file move.
                if del_oi != add_oi {
                    let old_path = overlays[del_oi].path.clone();
                    mutations.push((
                        add_oi,
                        add_ci,
                        ChangeType::Moved {
                            old_path: old_path.clone(),
                        },
                        MatchConfidence::Exact,
                    ));
                    // Mark the deleted side as Moved too (so formatter can show it).
                    mutations.push((
                        del_oi,
                        del_ci,
                        ChangeType::Moved { old_path },
                        MatchConfidence::Exact,
                    ));
                }
            } else {
                // Ambiguous — multiple candidates.
                let count = (del_locs.len() + add_locs.len()) as u32;
                for &(oi, ci) in add_locs {
                    mutations.push((oi, ci, ChangeType::Added, MatchConfidence::Ambiguous(count)));
                }
            }
        }
    }

    // Apply deferred mutations.
    for (oi, ci, change, confidence) in mutations {
        overlays[oi].symbol_changes[ci].change = change;
        overlays[oi].symbol_changes[ci].match_confidence = confidence;
    }
}

/// Warn when the same symbol name has multiple signature changes across files.
pub(crate) fn signature_warnings(overlays: &[FileOverlay]) -> Vec<String> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    for overlay in overlays {
        for change in &overlay.symbol_changes {
            if matches!(change.change, ChangeType::SignatureChanged) {
                *counts.entry(change.name.clone()).or_default() += 1;
            }
        }
    }

    counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(name, count)| {
            format!("warning: `{name}` signature changed in {count} locations — check callers")
        })
        .collect()
}

/// Scan a file for merge conflict markers and extract conflict blocks.
pub(crate) fn detect_conflicts(path: &Path) -> Vec<Conflict> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };

    let lines: Vec<&str> = content.lines().collect();
    let mut conflicts = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        if lines[i].starts_with("<<<<<<<") {
            let start = i;
            let mut separator = None;
            let mut end = None;

            // Find ======= and >>>>>>>.
            let mut j = i + 1;
            while j < lines.len() {
                if lines[j].starts_with("=======") {
                    separator = Some(j);
                } else if lines[j].starts_with(">>>>>>>") {
                    end = Some(j);
                    break;
                }
                j += 1;
            }

            if let (Some(sep), Some(e)) = (separator, end) {
                let ours = lines[start + 1..sep].join("\n");
                let theirs = lines[sep + 1..e].join("\n");

                // Find enclosing function via outline.
                let ft = detect_file_type(path);
                let enclosing_fn = if let FileType::Code(lang) = ft {
                    let entries = get_outline_entries(&content, lang);
                    find_enclosing_function(&entries, (start + 1) as u32)
                } else {
                    None
                };

                conflicts.push(Conflict {
                    line: (start + 1) as u32,
                    ours,
                    theirs,
                    enclosing_fn,
                });

                i = e + 1;
                continue;
            }
        }
        i += 1;
    }

    conflicts
}

// ---------------------------------------------------------------------------
// Per-status overlay builders
// ---------------------------------------------------------------------------

fn compute_modified(file_diff: &FileDiff, source: &DiffSource) -> FileOverlay {
    let path = &file_diff.path;
    let old_content = get_old_content(path, file_diff.old_path.as_deref(), source);
    let new_content = get_new_content(path, source);

    let ft = detect_file_type(path);
    let (symbol_changes, attributed_hunks) = if let FileType::Code(lang) = ft {
        let old_entries = get_outline_entries(&old_content, lang);
        let new_entries = get_outline_entries(&new_content, lang);

        if old_entries.is_empty() && new_entries.is_empty() {
            // No grammar support or empty outlines — skip symbol analysis.
            (Vec::new(), Vec::new())
        } else {
            let old_syms = build_diff_symbols(&old_entries, &old_content, lang);
            let new_syms = build_diff_symbols(&new_entries, &new_content, lang);
            let changes = match_symbols(&old_syms, &new_syms);
            let attributed = attribute_hunks(&file_diff.hunks, &changes);
            (changes, attributed)
        }
    } else {
        // Non-code file — no symbol analysis.
        (Vec::new(), Vec::new())
    };

    FileOverlay {
        path: path.clone(),
        symbol_changes,
        attributed_hunks,
        conflicts: Vec::new(),
        new_content: Some(new_content),
    }
}

fn compute_added(file_diff: &FileDiff, source: &DiffSource) -> FileOverlay {
    let path = &file_diff.path;
    let new_content = get_new_content(path, source);

    let symbol_changes = entries_to_changes(&new_content, path, &ChangeType::Added);

    FileOverlay {
        path: path.clone(),
        symbol_changes,
        attributed_hunks: Vec::new(),
        conflicts: Vec::new(),
        new_content: Some(new_content),
    }
}

fn compute_deleted(file_diff: &FileDiff, source: &DiffSource) -> FileOverlay {
    let path = &file_diff.path;
    let old_content = get_old_content(path, file_diff.old_path.as_deref(), source);

    let symbol_changes = entries_to_changes(&old_content, path, &ChangeType::Deleted);

    FileOverlay {
        path: path.clone(),
        symbol_changes,
        attributed_hunks: Vec::new(),
        conflicts: Vec::new(),
        new_content: None,
    }
}

fn compute_renamed(file_diff: &FileDiff, source: &DiffSource) -> FileOverlay {
    let path = &file_diff.path;
    let old_content = get_old_content(path, file_diff.old_path.as_deref(), source);
    let new_content = get_new_content(path, source);

    let ft = detect_file_type(path);
    let (symbol_changes, attributed_hunks) = if let FileType::Code(lang) = ft {
        let old_entries = get_outline_entries(&old_content, lang);
        let new_entries = get_outline_entries(&new_content, lang);
        let old_syms = build_diff_symbols(&old_entries, &old_content, lang);
        let new_syms = build_diff_symbols(&new_entries, &new_content, lang);
        let changes = match_symbols(&old_syms, &new_syms);
        let attributed = attribute_hunks(&file_diff.hunks, &changes);
        (changes, attributed)
    } else {
        (Vec::new(), Vec::new())
    };

    FileOverlay {
        path: path.clone(),
        symbol_changes,
        attributed_hunks,
        conflicts: Vec::new(),
        new_content: Some(new_content),
    }
}

// ---------------------------------------------------------------------------
// Content fetching helpers
// ---------------------------------------------------------------------------

fn get_old_content(path: &Path, old_path: Option<&Path>, source: &DiffSource) -> String {
    let effective_path = old_path.unwrap_or(path);
    let path_str = effective_path.to_string_lossy();

    match source {
        DiffSource::GitUncommitted | DiffSource::GitStaged => git_show(&format!("HEAD:{path_str}")),
        DiffSource::GitRef(r) => {
            if let Some((left, _)) = r.split_once("..") {
                git_show(&format!("{left}:{path_str}"))
            } else {
                git_show(&format!("{r}^:{path_str}"))
            }
        }
        DiffSource::Files(a, _) => std::fs::read_to_string(a).unwrap_or_default(),
        DiffSource::Patch(_) | DiffSource::Log(_) => String::new(),
    }
}

fn get_new_content(path: &Path, source: &DiffSource) -> String {
    let path_str = path.to_string_lossy();

    match source {
        DiffSource::GitUncommitted => std::fs::read_to_string(path).unwrap_or_default(),
        DiffSource::GitStaged => git_show(&format!(":{path_str}")),
        DiffSource::GitRef(r) => {
            if let Some((_, right)) = r.split_once("..") {
                git_show(&format!("{right}:{path_str}"))
            } else {
                git_show(&format!("{r}:{path_str}"))
            }
        }
        DiffSource::Files(_, b) => std::fs::read_to_string(b).unwrap_or_default(),
        DiffSource::Patch(_) | DiffSource::Log(_) => String::new(),
    }
}

fn git_show(spec: &str) -> String {
    Command::new("git")
        .args(["show", spec])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn get_entries_for_path(path: &Path, content: &str) -> Vec<OutlineEntry> {
    match detect_file_type(path) {
        FileType::Code(lang) => get_outline_entries(content, lang),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Hunk-to-function attribution
// ---------------------------------------------------------------------------

struct SymRange {
    name: String,
    start: u32,
    end: u32,
    is_deleted: bool,
}

/// For each symbol change that has a line range, find which diff lines from
/// the hunks fall within that symbol. Returns `(symbol_name, lines)` pairs.
fn attribute_hunks(
    hunks: &[super::Hunk],
    changes: &[SymbolChange],
) -> Vec<(String, Vec<DiffLine>)> {
    let mut result: Vec<(String, Vec<DiffLine>)> = Vec::new();

    let active_symbols: Vec<&SymbolChange> = changes
        .iter()
        .filter(|c| !matches!(c.change, ChangeType::Unchanged))
        .collect();

    if active_symbols.is_empty() {
        return result;
    }

    let mut sym_ranges: Vec<SymRange> = Vec::new();
    for change in &active_symbols {
        let start = change.line;
        let end = if let Some((old_size, new_size)) = change.size_delta {
            if matches!(change.change, ChangeType::Deleted) {
                start + old_size.saturating_sub(1)
            } else {
                start + new_size.saturating_sub(1)
            }
        } else {
            start
        };
        sym_ranges.push(SymRange {
            name: change.name.clone(),
            start,
            end,
            is_deleted: matches!(change.change, ChangeType::Deleted),
        });
    }

    // Pre-allocate buckets for each symbol.
    let mut buckets: Vec<Vec<DiffLine>> = (0..sym_ranges.len()).map(|_| Vec::new()).collect();

    for hunk in hunks {
        let mut old_line = hunk.old_start;
        let mut new_line = hunk.new_start;

        for diff_line in &hunk.lines {
            match diff_line.kind {
                DiffLineKind::Context => {
                    // Attribute by new-file line.
                    for (si, sr) in sym_ranges.iter().enumerate() {
                        if !sr.is_deleted && new_line >= sr.start && new_line <= sr.end {
                            buckets[si].push(DiffLine {
                                kind: diff_line.kind,
                                content: diff_line.content.clone(),
                            });
                        }
                    }
                    old_line += 1;
                    new_line += 1;
                }
                DiffLineKind::Added => {
                    // Attribute by new-file line.
                    for (si, sr) in sym_ranges.iter().enumerate() {
                        if !sr.is_deleted && new_line >= sr.start && new_line <= sr.end {
                            buckets[si].push(DiffLine {
                                kind: diff_line.kind,
                                content: diff_line.content.clone(),
                            });
                        }
                    }
                    new_line += 1;
                }
                DiffLineKind::Removed => {
                    // Attribute by old-file line.
                    for (si, sr) in sym_ranges.iter().enumerate() {
                        if sr.is_deleted && old_line >= sr.start && old_line <= sr.end {
                            buckets[si].push(DiffLine {
                                kind: diff_line.kind,
                                content: diff_line.content.clone(),
                            });
                        }
                    }
                    old_line += 1;
                }
            }
        }
    }

    // Collect non-empty buckets.
    for (si, lines) in buckets.into_iter().enumerate() {
        if !lines.is_empty() {
            result.push((sym_ranges[si].name.clone(), lines));
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Outline → SymbolChange helpers
// ---------------------------------------------------------------------------

/// Convert outline entries to symbol changes of a single type (Added or Deleted).
fn entries_to_changes(content: &str, path: &Path, change_type: &ChangeType) -> Vec<SymbolChange> {
    let entries = get_entries_for_path(path, content);
    let mut changes = Vec::new();
    collect_entries_recursive(&entries, change_type, &mut changes);
    changes
}

fn collect_entries_recursive(
    entries: &[OutlineEntry],
    change_type: &ChangeType,
    out: &mut Vec<SymbolChange>,
) {
    for entry in entries {
        // Skip imports/exports — not interesting for symbol-level diff.
        if matches!(entry.kind, OutlineKind::Import | OutlineKind::Export) {
            continue;
        }

        let (old_sig, new_sig) = match change_type {
            ChangeType::Added => (None, entry.signature.clone()),
            ChangeType::Deleted => (entry.signature.clone(), None),
            _ => (None, None),
        };

        out.push(SymbolChange {
            name: entry.name.clone(),
            kind: entry.kind,
            change: change_type.clone(),
            match_confidence: MatchConfidence::Exact,
            line: entry.start_line,
            old_sig,
            new_sig,
            size_delta: Some((
                entry.end_line.saturating_sub(entry.start_line) + 1,
                entry.end_line.saturating_sub(entry.start_line) + 1,
            )),
        });

        if !entry.children.is_empty() {
            collect_entries_recursive(&entry.children, change_type, out);
        }
    }
}

// ---------------------------------------------------------------------------
// Conflict helpers
// ---------------------------------------------------------------------------

/// Find the enclosing function for a given line number by walking the outline.
fn find_enclosing_function(entries: &[OutlineEntry], line: u32) -> Option<String> {
    for entry in entries {
        if line >= entry.start_line && line <= entry.end_line {
            // Check children first for more specific match.
            if let Some(child_name) = find_enclosing_function(&entry.children, line) {
                return Some(child_name);
            }
            if matches!(entry.kind, OutlineKind::Function) {
                return Some(entry.name.clone());
            }
        }
    }
    None
}
