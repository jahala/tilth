use std::collections::HashMap;
use std::path::Path;

use tilth_core::lang::detect_file_type_with_content;
use tilth_core::lang::outline::get_outline_entries;
use tilth_core::types::{FileType, OutlineEntry, OutlineKind};

use super::matching::{build_diff_symbols, match_symbols};
use super::{
    ChangeType, Conflict, DiffLine, DiffLineKind, FileDiff, FileOverlay, FileStatus, Hunk,
    MatchConfidence, Side, Sides, SymbolChange,
};
use crate::git::{Git, GitError};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build a structural overlay for a single file diff.
///
/// Reads the old and new content from `sides`, outlines both versions, runs
/// three-phase symbol matching, and gives every patch line to the innermost
/// symbol that holds it (see `attribute_lines`). Lines no symbol takes, and
/// all lines of a file with no outline, stay visible in `other_lines`.
/// `repo` anchors git commands and working-tree reads.
pub(crate) fn compute_overlay(file_diff: &FileDiff, sides: &Sides, repo: &Path) -> FileOverlay {
    let (added, removed) = count_changed_lines(&file_diff.hunks);
    let mut overlay = FileOverlay {
        path: file_diff.path.clone(),
        old_path: file_diff.old_path.clone(),
        symbol_changes: Vec::new(),
        other_lines: Vec::new(),
        added,
        removed,
        conflicts: Vec::new(),
        is_generated: file_diff.is_generated,
        is_binary: file_diff.is_binary,
    };

    // Binary or generated files: counts only, the formatter summarizes them.
    if file_diff.is_binary || file_diff.is_generated {
        return overlay;
    }

    let mut changes = match file_contents(file_diff, sides, repo) {
        // When either side cannot be read, matching would report every
        // symbol as added or deleted. Skip it; the lines still show.
        Err(_) => Vec::new(),
        Ok((old, new)) => {
            let old_path = file_diff.old_path.as_deref().unwrap_or(&file_diff.path);
            symbol_changes(old_path, &file_diff.path, &old, &new)
        }
    };
    // A whole added or deleted file lists its declarations, not its imports.
    if matches!(file_diff.status, FileStatus::Added | FileStatus::Deleted) {
        changes.retain(|c| !matches!(c.kind, OutlineKind::Import | OutlineKind::Export));
    }
    overlay.other_lines = attribute_lines(&file_diff.hunks, &mut changes);
    overlay.symbol_changes = changes;
    overlay
}

/// Cross-file move detection: match Deleted symbols in one file with Added
/// symbols in another by kind and qualified name, so a method moves only
/// within its own type. Unique pairs become `Moved{old_path}`.
pub(crate) fn cross_file_matching(overlays: &mut [FileOverlay]) {
    // Collect all Deleted and Added symbols with their overlay index + change index.
    let mut deleted: HashMap<(OutlineKind, String), Vec<(usize, usize)>> = HashMap::new();
    let mut added: HashMap<(OutlineKind, String), Vec<(usize, usize)>> = HashMap::new();

    for (oi, overlay) in overlays.iter().enumerate() {
        for (ci, change) in overlay.symbol_changes.iter().enumerate() {
            match &change.change {
                ChangeType::Deleted => {
                    deleted
                        .entry((change.kind, change.qualified_name()))
                        .or_default()
                        .push((oi, ci));
                }
                ChangeType::Added => {
                    added
                        .entry((change.kind, change.qualified_name()))
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

    // Each change takes at most one mutation, so their order does not matter.
    for (oi, ci, change, confidence) in mutations {
        overlays[oi].symbol_changes[ci].change = change;
        overlays[oi].symbol_changes[ci].match_confidence = confidence;
    }
}

/// Warn when the same symbol has its signature changed in several files.
/// Sorted, so the warnings come out in the same order every run.
pub(crate) fn signature_warnings<'a>(
    overlays: impl IntoIterator<Item = &'a FileOverlay>,
) -> Vec<String> {
    let mut counts: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    for overlay in overlays {
        for change in &overlay.symbol_changes {
            if matches!(change.change, ChangeType::SignatureChanged) {
                *counts.entry(change.qualified_name()).or_default() += 1;
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
pub(crate) fn detect_conflicts(path: &Path, repo: &Path) -> Vec<Conflict> {
    let disk = repo.join(path);
    let Ok(content) = tilth_core::source::read_to_string(&disk) else {
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
                let ft = detect_file_type_with_content(path, &content);
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

/// Added and removed lines in a file's patch.
pub(crate) fn count_changed_lines(hunks: &[Hunk]) -> (usize, usize) {
    let lines = || hunks.iter().flat_map(|h| &h.lines);
    let count = |kind| lines().filter(|l| l.kind == kind).count();
    (count(DiffLineKind::Added), count(DiffLineKind::Removed))
}

// ---------------------------------------------------------------------------
// Symbols
// ---------------------------------------------------------------------------

/// Match the symbols of the old and new content of one file. Each side is
/// read in its own language, from its own path and content: a rename can
/// change the extension, and a `.h` can gain or lose C++. A side that is not
/// code, or has no outline, has no symbols; when neither has any, there is
/// nothing to match.
fn symbol_changes(old_path: &Path, path: &Path, old: &str, new: &str) -> Vec<SymbolChange> {
    let symbols = |path: &Path, content: &str| match detect_file_type_with_content(path, content) {
        FileType::Code(lang) if !content.is_empty() => Some(build_diff_symbols(content, lang)),
        _ => None,
    };
    let (old_syms, new_syms) = (symbols(old_path, old), symbols(path, new));
    if old_syms.is_none() && new_syms.is_none() {
        return Vec::new();
    }
    match_symbols(&old_syms.unwrap_or_default(), &new_syms.unwrap_or_default())
}

/// Give each patch line to the innermost symbol whose span holds it: a
/// removed line by its old line number, any other line by its new one. A
/// changed line goes only to a changed symbol; a context line goes to any
/// symbol, so the context inside an unchanged symbol stays with it. Returns
/// the lines no symbol takes. Within one hunk, a symbol (or the outside)
/// that receives only context lines receives none, so an unchanged symbol
/// never receives any.
fn attribute_lines(hunks: &[Hunk], changes: &mut [SymbolChange]) -> Vec<DiffLine> {
    let mut other = Vec::new();
    for hunk in hunks {
        let mut groups: Vec<(Option<usize>, Vec<DiffLine>)> = Vec::new();
        for line in &hunk.lines {
            let owner = innermost_owner(changes, line);
            match groups.iter_mut().find(|(o, _)| *o == owner) {
                Some((_, lines)) => lines.push(line.clone()),
                None => groups.push((owner, vec![line.clone()])),
            }
        }
        for (owner, lines) in groups {
            if lines.iter().all(|l| l.kind == DiffLineKind::Context) {
                continue;
            }
            match owner {
                Some(i) => changes[i].lines.extend(lines),
                None => other.extend(lines),
            }
        }
    }
    other
}

fn innermost_owner(changes: &[SymbolChange], line: &DiffLine) -> Option<usize> {
    changes
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            line.kind == DiffLineKind::Context || !matches!(c.change, ChangeType::Unchanged)
        })
        .filter_map(|(i, c)| {
            let (start, end) = if line.kind == DiffLineKind::Removed {
                c.old_span?
            } else {
                c.new_span?
            };
            (start <= line.line && line.line <= end).then_some((end - start, i))
        })
        .min()
        .map(|(_, i)| i)
}

// ---------------------------------------------------------------------------
// Content fetching helpers
// ---------------------------------------------------------------------------

/// The old and new content of a file. An added file has no old side and a
/// deleted file no new side; both read as empty without asking git.
fn file_contents(
    file_diff: &FileDiff,
    sides: &Sides,
    repo: &Path,
) -> Result<(String, String), String> {
    let old = if file_diff.status == FileStatus::Added {
        String::new()
    } else {
        let old_path = file_diff.old_path.as_deref().unwrap_or(&file_diff.path);
        read_side(&sides.old, old_path, repo)?
    };
    let new = if file_diff.status == FileStatus::Deleted {
        String::new()
    } else {
        read_side(&sides.new, &file_diff.path, repo)?
    };
    Ok((old, new))
}

/// Read `path` (repository-relative) from one side of a diff.
fn read_side(side: &Side, path: &Path, repo: &Path) -> Result<String, String> {
    let path_str = path.to_string_lossy();
    match side {
        Side::Rev(rev) => git_show(&format!("{rev}:{path_str}"), repo).map_err(|e| e.to_string()),
        Side::Index => git_show(&format!(":{path_str}"), repo).map_err(|e| e.to_string()),
        Side::WorkingTree => {
            // Git emits paths relative to the repository top.
            let disk = repo.join(path);
            tilth_core::source::read_to_string(&disk)
                .map_err(|e| format!("read {}: {e}", disk.display()))
        }
        Side::File(file) => tilth_core::source::read_to_string(file)
            .map_err(|e| format!("read {}: {e}", file.display())),
        Side::Unavailable => Ok(String::new()),
    }
}

/// The content `spec` (`<rev>:<path>`, or `:<path>` for the index) names.
pub(super) fn git_show(spec: &str, repo: &Path) -> Result<String, GitError> {
    Git::new(repo, "show").revision(spec).run()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{resolve_sides, DiffSource};

    #[test]
    fn working_tree_side_reads_the_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sample.rs");
        std::fs::write(&file, "fn worktree_only() {}\n").unwrap();

        // A single ref (no `..`) diffs against the working tree, so the new
        // content must come from the file on disk — not `git show HEAD:<path>`,
        // which would return empty here and silently mis-attribute the diff.
        let content = read_side(&Side::WorkingTree, &file, dir.path()).expect("working-tree read");
        assert!(content.contains("worktree_only"), "{content:?}");
    }

    #[test]
    fn revision_ranges_pick_both_sides() {
        // None of these ranges runs git, so the repository is never entered.
        let repo = Path::new("/nonexistent");
        let sides = |r: &str| resolve_sides(&DiffSource::GitRef(r.to_string()), repo).unwrap();
        // A bare ref compares that ref with the working tree.
        assert_eq!(
            sides("HEAD~1"),
            Sides {
                old: Side::Rev("HEAD~1".into()),
                new: Side::WorkingTree
            }
        );
        // A range reads the committed blob at its right end, not the working
        // tree; an empty end means HEAD.
        assert_eq!(
            sides("HEAD..feature"),
            Sides {
                old: Side::Rev("HEAD".into()),
                new: Side::Rev("feature".into())
            }
        );
        assert_eq!(
            sides("main.."),
            Sides {
                old: Side::Rev("main".into()),
                new: Side::Rev("HEAD".into())
            }
        );
    }

    #[test]
    fn unreadable_side_skips_symbol_analysis_but_keeps_lines() {
        // When one side cannot be read, matching would report every symbol
        // of the other side as added or deleted. The overlay has no symbols
        // instead, and the patch lines stay visible.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("foo.rs");
        std::fs::write(&file, "fn new_fn() {}\n").unwrap();
        let missing = dir.path().join("does_not_exist.rs");
        let sides = Sides {
            old: Side::File(missing),
            new: Side::File(file.clone()),
        };
        assert!(read_side(&sides.old, &file, dir.path()).is_err());

        let file_diff = FileDiff {
            path: file,
            old_path: None,
            status: FileStatus::Modified,
            hunks: vec![Hunk {
                old_start: 1,
                old_count: 1,
                new_start: 1,
                new_count: 1,
                lines: vec![
                    DiffLine {
                        kind: DiffLineKind::Removed,
                        line: 1,
                        content: "fn old_fn() {}".into(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Added,
                        line: 1,
                        content: "fn new_fn() {}".into(),
                    },
                ],
            }],
            is_generated: false,
            is_binary: false,
        };
        let overlay = compute_overlay(&file_diff, &sides, dir.path());
        assert!(overlay.symbol_changes.is_empty(), "{overlay:?}");
        assert_eq!(overlay.other_lines.len(), 2, "{overlay:?}");
        assert_eq!((overlay.added, overlay.removed), (1, 1));
    }

    #[test]
    fn count_changed_lines_skips_context() {
        let line = |kind| DiffLine {
            kind,
            line: 1,
            content: String::new(),
        };
        let hunk = |lines| Hunk {
            old_start: 1,
            old_count: 1,
            new_start: 1,
            new_count: 1,
            lines,
        };
        let hunks = vec![
            hunk(vec![
                line(DiffLineKind::Added),
                line(DiffLineKind::Added),
                line(DiffLineKind::Removed),
                line(DiffLineKind::Context),
            ]),
            hunk(vec![line(DiffLineKind::Removed)]),
        ];
        assert_eq!(count_changed_lines(&hunks), (2, 2));
    }

    /// Each side of a diff is parsed in its own language. A header that
    /// loses its C++ was parsed as C on both sides, so its old class read as
    /// a garbled function and was reported under the wrong kind.
    #[test]
    fn each_side_is_parsed_in_its_own_language() {
        let old = "#pragma once\nclass Shape {\n public:\n  int area() { return 0; }\n};\n";
        let new = "#pragma once\nstruct shape { int sides; };\nint area(struct shape *s);\n";
        let changes = symbol_changes(Path::new("shape.h"), Path::new("shape.h"), old, new);
        let shape = changes.iter().find(|c| c.name == "Shape").expect("Shape");
        assert!(
            shape.kind == OutlineKind::Class && matches!(shape.change, ChangeType::Deleted),
            "{changes:#?}"
        );
    }
}
