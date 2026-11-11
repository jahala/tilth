use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::edit::Edit;
use tilth_core::lang::detect_file_type_with_content;
use tilth_core::lang::outline::get_outline_entries;
use tilth_core::search::callers::find_callers_batch;
use tilth_core::types::{is_test_file, FileType, OutlineEntry, OutlineKind};

pub(crate) struct TouchedSymbol {
    name: String,
}

/// Returns symbols whose definitions overlap the given edit ranges.
pub(crate) fn touched_symbols(edits: &[Edit], entries: &[OutlineEntry]) -> Vec<TouchedSymbol> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut result: Vec<TouchedSymbol> = Vec::new();

    for entry in entries {
        collect_touched(edits, entry, &mut seen, &mut result);
        for child in &entry.children {
            collect_touched(edits, child, &mut seen, &mut result);
        }
    }

    result
}

fn collect_touched(
    edits: &[Edit],
    entry: &OutlineEntry,
    seen: &mut HashSet<String>,
    result: &mut Vec<TouchedSymbol>,
) {
    if seen.contains(&entry.name) {
        return;
    }

    let triggered = edits.iter().any(|edit| match entry.kind {
        OutlineKind::Function => {
            // Signature region: start_line through start_line+3 covers attributes,
            // fn keyword, parameters, and return type on separate lines.
            // Tree-sitter includes #[attr] in the node, so start_line may be
            // an attribute line rather than the `fn` keyword.
            let sig_start = entry.start_line as usize;
            let sig_end = (entry.start_line as usize + 3).min(entry.end_line as usize);
            edit.start_line <= sig_end && sig_start <= edit.end_line
        }
        _ => false,
    });

    if triggered {
        let name = entry.name.clone();
        seen.insert(name.clone());
        result.push(TouchedSymbol { name });
    }
}

/// Computes blast radius for a set of edits on `path`.
///
/// Returns a formatted string describing external callers of any definitions
/// touched by the edits. Returns `None` if no definitions were touched or no
/// external callers exist.
pub(crate) fn blast_radius(
    path: &Path,
    edits: &[Edit],
    scope: &Path,
    bloom: &tilth_core::index::bloom::BloomFilterCache,
) -> Option<String> {
    let content = tilth_core::source::read_source(path).ok()?;

    let FileType::Code(lang) = detect_file_type_with_content(path, &content) else {
        return None;
    };

    let entries = get_outline_entries(&content, lang);
    let touched = touched_symbols(edits, &entries);
    if touched.is_empty() {
        return None;
    }

    let symbol_names: HashSet<String> = touched.iter().map(|t| t.name.clone()).collect();

    // Calls from the edited file itself are not part of its blast radius.
    let canonical = path.canonicalize().ok()?;
    let files: Vec<FileCallers> = find_callers_batch(&symbol_names, scope, bloom, None, |sites| {
        let mut file = FileCallers::default();
        for (symbol, m) in sites {
            if m.path == canonical {
                continue;
            }
            if is_test_file(&m.path) {
                file.test_path.get_or_insert_with(|| m.path.clone());
                file.tests += 1;
                continue;
            }
            let entry = file.by_symbol.entry(symbol).or_default();
            entry.total += 1;
            if entry.first.len() < PROD_CALLERS_LISTED {
                entry.first.push((m.path, m.line, m.calling_function));
            }
        }
        file
    })
    .ok()?
    .per_file;

    if files.iter().all(|f| f.by_symbol.is_empty() && f.tests == 0) {
        return None;
    }

    Some(format_blast_radius(&touched, &files, scope))
}

/// Production callers listed per symbol before the rest are counted.
const PROD_CALLERS_LISTED: usize = 8;
/// Test files named in the test summary before the rest are counted.
const TEST_FILES_LISTED: usize = 5;

/// One file's calls to the touched symbols: production calls counted per
/// symbol with the first few kept, test calls counted.
#[derive(Default)]
struct FileCallers {
    by_symbol: HashMap<String, SymbolCallers>,
    /// Set when the file is a test file with at least one call.
    test_path: Option<PathBuf>,
    tests: usize,
}

#[derive(Default)]
struct SymbolCallers {
    total: usize,
    /// (path, line, calling function) of the first calls, by line.
    first: Vec<(PathBuf, u32, String)>,
}

/// `files` come in path order, so a symbol's listed callers are its first
/// by (path, line).
fn format_blast_radius(touched: &[TouchedSymbol], files: &[FileCallers], scope: &Path) -> String {
    let mut out = String::from("\n── blast radius ──\n");
    let rel = |p: &Path| p.strip_prefix(scope).unwrap_or(p).display().to_string();

    // Emit per-symbol prod callers in touched order (preserves definition order).
    for ts in touched {
        let total: usize = files
            .iter()
            .filter_map(|f| f.by_symbol.get(&ts.name))
            .map(|c| c.total)
            .sum();
        if total == 0 {
            continue;
        }

        let _ = writeln!(
            out,
            "{}: {} caller{}",
            ts.name,
            total,
            if total == 1 { "" } else { "s" }
        );

        let listed = files
            .iter()
            .filter_map(|f| f.by_symbol.get(&ts.name))
            .flat_map(|c| c.first.iter())
            .take(PROD_CALLERS_LISTED);
        for (path, line, calling_function) in listed {
            let _ = writeln!(out, "  {}:{}  {}", rel(path), line, calling_function);
        }

        if total > PROD_CALLERS_LISTED {
            let _ = writeln!(out, "  ... and {} more", total - PROD_CALLERS_LISTED);
        }
    }

    // Test summary — test files by how many calls they make, across all symbols.
    let mut test_files: Vec<(String, usize)> = files
        .iter()
        .filter_map(|f| f.test_path.as_ref().map(|p| (rel(p), f.tests)))
        .collect();

    if !test_files.is_empty() {
        test_files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let mut summary: String = test_files
            .iter()
            .take(TEST_FILES_LISTED)
            .map(|(f, c)| format!("{f} ({c})"))
            .collect::<Vec<_>>()
            .join(", ");
        let unlisted = test_files.len().saturating_sub(TEST_FILES_LISTED);
        if unlisted > 0 {
            let _ = write!(
                summary,
                ", and {unlisted} more file{}",
                if unlisted == 1 { "" } else { "s" }
            );
        }

        let _ = writeln!(out, "tests: {summary}");
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilth_core::types::OutlineKind;

    fn make_edit(start: usize, end: usize) -> Edit {
        Edit {
            start_line: start,
            start_hash: 0,
            end_line: end,
            end_hash: 0,
            content: String::new(),
        }
    }

    fn make_fn(name: &str, start: u32, end: u32) -> OutlineEntry {
        OutlineEntry {
            kind: OutlineKind::Function,
            name: name.to_string(),
            start_line: start,
            end_line: end,
            signature: None,
            children: Vec::new(),
            doc: None,
            local_name: None,
        }
    }

    fn make_entry(kind: OutlineKind, name: &str, start: u32, end: u32) -> OutlineEntry {
        OutlineEntry {
            kind,
            name: name.to_string(),
            start_line: start,
            end_line: end,
            signature: None,
            children: Vec::new(),
            doc: None,
            local_name: None,
        }
    }

    #[test]
    fn signature_edit_triggers() {
        let entries = vec![make_fn("foo", 10, 30)];
        let edits = vec![make_edit(10, 10)];
        let result = touched_symbols(&edits, &entries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "foo");
    }

    #[test]
    fn signature_end_triggers() {
        // Edit at line 13 (start_line + 3) should still trigger.
        let entries = vec![make_fn("foo", 10, 30)];
        let edits = vec![make_edit(13, 13)];
        let result = touched_symbols(&edits, &entries);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn body_edit_does_not_trigger() {
        let entries = vec![make_fn("foo", 10, 30)];
        let edits = vec![make_edit(20, 25)];
        let result = touched_symbols(&edits, &entries);
        assert!(result.is_empty());
    }

    #[test]
    fn struct_does_not_trigger() {
        let entries = vec![make_entry(OutlineKind::Struct, "Bar", 5, 20)];
        let edits = vec![make_edit(5, 5)];
        let result = touched_symbols(&edits, &entries);
        assert!(result.is_empty());
    }

    #[test]
    fn child_method_triggers() {
        let mut class = make_entry(OutlineKind::Class, "MyClass", 1, 50);
        class.children.push(make_fn("method", 10, 25));
        let entries = vec![class];
        let edits = vec![make_edit(10, 12)];
        let result = touched_symbols(&edits, &entries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "method");
    }

    #[test]
    fn dedup_same_symbol() {
        let entries = vec![make_fn("foo", 10, 30)];
        // Two edits both hitting the signature.
        let edits = vec![make_edit(10, 10), make_edit(11, 12)];
        let result = touched_symbols(&edits, &entries);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn short_function_clamps_sig_end() {
        // Function with only 2 lines: sig_end = min(10+3, 11) = 11.
        let entries = vec![make_fn("tiny", 10, 11)];
        let edits = vec![make_edit(11, 11)];
        let result = touched_symbols(&edits, &entries);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn no_edits_no_results() {
        let entries = vec![make_fn("foo", 10, 30)];
        let result = touched_symbols(&[], &entries);
        assert!(result.is_empty());
    }

    #[test]
    fn no_entries_no_results() {
        let edits = vec![make_edit(10, 10)];
        let result = touched_symbols(&edits, &[]);
        assert!(result.is_empty());
    }
}
