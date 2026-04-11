use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use crate::edit::Edit;
use crate::read::detect_file_type;
use crate::search::callees::get_outline_entries;
use crate::search::callers::{find_callers_batch, CallerMatch};
use crate::types::{is_test_file, FileType, OutlineEntry, OutlineKind};

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
    bloom: &crate::index::bloom::BloomFilterCache,
) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;

    let FileType::Code(lang) = detect_file_type(path) else {
        return None;
    };

    let entries = get_outline_entries(&content, lang);
    let touched = touched_symbols(edits, &entries);
    if touched.is_empty() {
        return None;
    }

    let symbol_names: HashSet<String> = touched.iter().map(|t| t.name.clone()).collect();

    let callers = find_callers_batch(&symbol_names, scope, bloom, None).ok()?;

    let canonical = path.canonicalize().ok()?;
    let callers: Vec<(String, CallerMatch)> = callers
        .into_iter()
        .filter(|(_, m)| m.path != canonical)
        .collect();

    if callers.is_empty() {
        return None;
    }

    Some(format_blast_radius(&touched, &callers, scope))
}

fn format_blast_radius(
    touched: &[TouchedSymbol],
    callers: &[(String, CallerMatch)],
    scope: &Path,
) -> String {
    let mut out = String::from("\n── blast radius ──\n");

    // Group callers by symbol name, split into prod and test.
    let mut by_symbol: HashMap<&str, (Vec<&CallerMatch>, Vec<&CallerMatch>)> = HashMap::new();
    for (sym, m) in callers {
        let entry = by_symbol.entry(sym.as_str()).or_default();
        if is_test_file(&m.path) {
            entry.1.push(m);
        } else {
            entry.0.push(m);
        }
    }

    // Emit per-symbol prod callers in touched order (preserves definition order).
    for ts in touched {
        let Some((prod, _)) = by_symbol.get(ts.name.as_str()) else {
            continue;
        };
        if prod.is_empty() {
            continue;
        }

        let _ = writeln!(
            out,
            "{}: {} caller{}",
            ts.name,
            prod.len(),
            if prod.len() == 1 { "" } else { "s" }
        );

        for m in prod.iter().take(8) {
            let rel = m
                .path
                .strip_prefix(scope)
                .unwrap_or(&m.path)
                .display()
                .to_string();
            let _ = writeln!(out, "  {}:{}  {}", rel, m.line, m.calling_function);
        }

        if prod.len() > 8 {
            let _ = writeln!(out, "  ... and {} more", prod.len() - 8);
        }
    }

    // Test summary — group all test callers by file, across all symbols.
    let mut test_counts: HashMap<String, usize> = HashMap::new();
    for (_, m) in callers {
        if is_test_file(&m.path) {
            let rel = m
                .path
                .strip_prefix(scope)
                .unwrap_or(&m.path)
                .display()
                .to_string();
            *test_counts.entry(rel).or_insert(0) += 1;
        }
    }

    if !test_counts.is_empty() {
        let mut files: Vec<(&String, usize)> = test_counts.iter().map(|(k, &v)| (k, v)).collect();
        files.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));

        let shown = files.iter().take(5);
        let summary: String = shown
            .map(|(f, c)| format!("{f} ({c})"))
            .collect::<Vec<_>>()
            .join(", ");

        let _ = writeln!(out, "tests: {summary}");
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::OutlineKind;

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
