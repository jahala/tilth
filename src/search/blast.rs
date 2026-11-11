use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::edit::Edit;
use tilth_core::lang::detect_file_type_with_content;
use tilth_core::lang::outline::get_outline_entries;
use tilth_core::search::callers::find_callers_batch;
use tilth_core::types::{is_test_file, FileType, OutlineEntry, OutlineKind};

pub struct TouchedSymbol {
    name: String,
}

/// Returns symbols whose definitions overlap the given edit ranges.
pub fn touched_symbols(edits: &[Edit], entries: &[OutlineEntry]) -> Vec<TouchedSymbol> {
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
pub fn blast_radius(
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
    let walk = find_callers_batch(
        &symbol_names,
        scope,
        bloom,
        None,
        Some(BLAST_STOP_AFTER),
        |sites| {
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
        },
    )
    .ok()?;
    let files = walk.per_file;

    if files.iter().all(|f| f.by_symbol.is_empty() && f.tests == 0) {
        return None;
    }

    Some(format_blast_radius(
        &touched,
        &files,
        walk.reached_bound,
        scope,
    ))
}

/// Call sites after which a blast walk stops. Blast runs on every write
/// that touches a definition, so it must stay fast on a huge tree; past
/// this many sites its counts are labelled lower bounds.
pub const BLAST_STOP_AFTER: usize = 50;

/// `n` as a count, prefixed "at least" when the walk stopped early.
fn lower_bound(n: usize, partial: bool) -> String {
    if partial {
        format!("at least {n}")
    } else {
        n.to_string()
    }
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
/// by (path, line). `partial` says the walk stopped at `BLAST_STOP_AFTER`,
/// so every count is a lower bound.
fn format_blast_radius(
    touched: &[TouchedSymbol],
    files: &[FileCallers],
    partial: bool,
    scope: &Path,
) -> String {
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
            lower_bound(total, partial),
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
            let more = lower_bound(total - PROD_CALLERS_LISTED, partial);
            let _ = writeln!(out, "  ... and {more} more");
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
                ", and {} more file{}",
                lower_bound(unlisted, partial),
                if unlisted == 1 { "" } else { "s" }
            );
        } else if partial {
            summary.push_str(", and possibly more files");
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

    /// A tree with `target` defined in `lib.rs`, `files` production files
    /// that call it three times each, and one test file that calls it
    /// three times. Returns the blast radius of an edit to `target`'s
    /// signature.
    fn blast_of_tree(files: usize) -> String {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib.rs");
        std::fs::write(&lib, "pub fn target(x: u32) -> u32 {\n    x\n}\n").unwrap();
        let calls = "fn caller() {\n    target(1);\n    target(2);\n    target(3);\n}\n";
        for i in 0..files {
            std::fs::write(dir.path().join(format!("c{i:02}.rs")), calls).unwrap();
        }
        std::fs::create_dir(dir.path().join("__tests__")).unwrap();
        std::fs::write(dir.path().join("__tests__/t.rs"), calls).unwrap();

        let bloom = tilth_core::index::bloom::BloomFilterCache::new();
        let scope = dir.path().canonicalize().unwrap();
        blast_radius(&lib, &[make_edit(1, 1)], &scope, &bloom).expect("blast radius")
    }

    #[test]
    fn a_blast_under_the_bound_gives_exact_counts() {
        // 4 × 3 = 12 production calls and 3 test calls, under the bound.
        let out = blast_of_tree(4);
        assert!(out.contains("target: 12 callers\n"), "{out}");
        assert!(out.contains("tests: __tests__/t.rs (3)\n"), "{out}");
        assert!(!out.contains("at least"), "{out}");
        assert!(!out.contains("possibly"), "{out}");
    }

    #[test]
    fn a_blast_that_hits_the_bound_says_its_counts_are_lower_bounds() {
        // 40 × 3 = 120 production calls, well past the bound. A write stops
        // the walk there to stay fast, so the count must not read as exact.
        let out = blast_of_tree(40);
        let line = out
            .lines()
            .find(|l| l.starts_with("target: "))
            .unwrap_or_else(|| panic!("no target line in {out}"));
        let n: usize = line
            .strip_prefix("target: at least ")
            .and_then(|rest| rest.strip_suffix(" callers"))
            .unwrap_or_else(|| panic!("count not labelled a lower bound: {line}"))
            .parse()
            .unwrap();
        assert!((PROD_CALLERS_LISTED..=120).contains(&n), "{out}");
        assert!(out.contains("  ... and at least "), "{out}");
    }
}
