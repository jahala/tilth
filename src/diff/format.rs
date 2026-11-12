use std::fmt::Write;
use std::path::Path;

use crate::budget;
use crate::format::{noun, rel};
use crate::types::estimate_tokens;

use super::{
    ChangeType, CommitSummary, Conflict, DiffLine, DiffLineKind, FileOverlay, MatchConfidence,
    SymbolChange,
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Format a high-level overview of all changed files with per-symbol markers.
///
/// Layout:
/// ```text
/// # Diff: {source_label} — N files, M modified, K added (~X tokens)
///
/// ## path (N symbols)
///   [~:sig]  name — fn name(old) → fn name(new)    L42
///   [~]      Type::name                            L88  (body, 44→51 lines)
///   [+]      name                                  L120 (new, 18 lines)
///
/// ## package-lock.json (generated, N lines changed — summarized)
///
/// ## [~] path:Type::name — body changed (L88-138)
///   -  90| removed line (old-file line number)
///   +  90| added line
/// ```
///
/// The sections at the end belong to the first `expand` changed symbols, as
/// [`write_expanded`] picks them. They come after the warnings, so a budget
/// cut takes them before any file or warning.
pub(crate) fn format_overview(
    overlays: &[FileOverlay],
    warnings: &[String],
    source_label: &str,
    expand: usize,
    budget: Option<u64>,
) -> String {
    let scope = Path::new(".");

    // Count stats for header.
    let file_count = overlays.len();
    let mut fn_modified: usize = 0;
    let mut fn_added: usize = 0;
    for overlay in overlays {
        for change in &overlay.symbol_changes {
            match &change.change {
                ChangeType::BodyChanged | ChangeType::SignatureChanged => fn_modified += 1,
                ChangeType::Added => fn_added += 1,
                _ => {}
            }
        }
    }

    let mut out = String::new();

    // Placeholder header — token count is patched below.
    let _ = writeln!(
        out,
        "# Diff: {} — {} {}, {} modified, {} added",
        source_label,
        file_count,
        noun(file_count, "file", "files"),
        fn_modified,
        fn_added,
    );

    for overlay in overlays {
        let rel_path = rel(&overlay.path, scope);

        let _ = writeln!(out);

        if overlay.is_binary {
            let _ = writeln!(out, "## {rel_path} (binary)");
            continue;
        }

        if overlay.is_generated {
            let changed_lines = overlay.added + overlay.removed;
            let _ = writeln!(
                out,
                "## {rel_path} (generated, {changed_lines} {} changed — summarized)",
                noun(changed_lines, "line", "lines")
            );
            continue;
        }

        // Normal file — show non-Unchanged symbol changes.
        let visible: Vec<&SymbolChange> = overlay
            .symbol_changes
            .iter()
            .filter(|c| !matches!(c.change, ChangeType::Unchanged))
            .collect();

        let sym_count = visible.len();
        let symbols = noun(sym_count, "symbol", "symbols");
        let _ = writeln!(out, "## {rel_path} ({sym_count} {symbols})");

        for change in &visible {
            let line = format_symbol_line(change);
            let _ = writeln!(out, "  {line}");
        }
    }

    // Warnings at the bottom, then the expanded symbols.
    write_warnings(&mut out, warnings);
    write_expanded(&mut out, overlays, scope, expand);

    // Patch in the token count now that we know the full size.
    let token_count = estimate_tokens(out.len() as u64);
    if let Some(nl) = out.find('\n') {
        let new_header = format!(
            "# Diff: {} — {} {}, {} modified, {} added (~{} tokens)",
            source_label,
            file_count,
            noun(file_count, "file", "files"),
            fn_modified,
            fn_added,
            token_count,
        );
        out.replace_range(..nl, &new_header);
    }

    if let Some(b) = budget {
        budget::apply(&out, b)
    } else {
        out
    }
}

/// Format detailed view of a single file's diff, with per-symbol sections.
///
/// Layout:
/// ```text
/// # Diff: path — N symbols touched, +X/−Y lines
///
/// ## [~:sig] Type::name — signature changed (L42-93)
///   BEFORE: old_sig
///   AFTER:  new_sig
///   +  48| added line
///   -  49| removed line (old-file line number)
///      50| context line
///
/// ## [ ] unchanged_name (L100-120, unchanged)
///
/// ## outside symbols
///   +   3| use std::io;
/// ```
pub(crate) fn format_file_detail(
    overlay: &FileOverlay,
    warnings: &[String],
    budget: Option<u64>,
) -> String {
    let scope = Path::new(".");
    let rel_path = rel(&overlay.path, scope);

    let sym_touched: usize = overlay
        .symbol_changes
        .iter()
        .filter(|c| !matches!(c.change, ChangeType::Unchanged))
        .count();

    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Diff: {rel_path} — {sym_touched} {} touched, +{}/\u{2212}{} lines",
        noun(sym_touched, "symbol", "symbols"),
        overlay.added,
        overlay.removed
    );

    for change in &overlay.symbol_changes {
        let (line, end_line) = change.span();
        let name = change.qualified_name();

        if matches!(change.change, ChangeType::Unchanged) {
            let _ = writeln!(out, "\n## [ ] {name} (L{line}-{end_line}, unchanged)");
            continue;
        }

        write_change(&mut out, &name, change);
    }

    // Lines no symbol holds: all of a text file, or imports and comments
    // between symbols. A file with no symbols needs no heading for them.
    if !overlay.other_lines.is_empty() {
        if overlay.symbol_changes.is_empty() {
            out.push('\n');
        } else {
            out.push_str("\n## outside symbols\n");
        }
        write_diff_lines(&mut out, &overlay.other_lines);
    }

    write_warnings(&mut out, warnings);

    if let Some(b) = budget {
        budget::apply(&out, b)
    } else {
        out
    }
}

/// Format detailed view of a single named function within a file's diff.
///
/// `fn_name` is a qualified name (`Type::method`, or `Type.method`) or a bare
/// one. A bare name that several changed symbols share is ambiguous. An
/// unchanged symbol of the name is shown only when no changed one has it.
pub(crate) fn format_function_detail(overlay: &FileOverlay, fn_name: &str) -> String {
    let wanted = fn_name.replace('.', "::");
    let qualified: Vec<&SymbolChange> = overlay
        .symbol_changes
        .iter()
        .filter(|c| c.qualified_name() == wanted)
        .collect();
    let mut candidates: Vec<&SymbolChange> = if qualified.is_empty() {
        overlay
            .symbol_changes
            .iter()
            .filter(|c| c.name == fn_name)
            .collect()
    } else {
        qualified
    };
    if candidates
        .iter()
        .any(|c| !matches!(c.change, ChangeType::Unchanged))
    {
        candidates.retain(|c| !matches!(c.change, ChangeType::Unchanged));
    }

    let change = match candidates.as_slice() {
        [] => {
            return format!(
                "# Diff: {} — `{fn_name}` not found in diff",
                overlay.path.display()
            );
        }
        [one] => *one,
        many => {
            let names: Vec<String> = many.iter().map(|c| c.qualified_name()).collect();
            return format!(
                "# Diff: {} — `{fn_name}` is ambiguous: {}. Name one as path:Type::{fn_name}",
                overlay.path.display(),
                names.join(", ")
            );
        }
    };

    let marker = format_marker(&change.change, &change.match_confidence);
    let label = change_type_label(&change.change);
    let (line, end_line) = change.span();
    let name = change.qualified_name();

    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Diff: {} — {marker} {name} (L{line}-{end_line})",
        overlay.path.display()
    );
    let _ = writeln!(out, "  {name} ({label})");
    write_signatures(&mut out, change);
    write_diff_lines(&mut out, &change.lines);

    out
}

/// Format merge conflict blocks for a file.
///
/// Layout:
/// ```text
/// # Conflicts: N in path
///
/// ## fn enclosing_name (LN)
///   OURS:
///     ...
///   THEIRS:
///     ...
/// ```
pub(crate) fn format_conflicts(conflicts: &[Conflict], path: &Path) -> String {
    let scope = Path::new(".");
    let rel_path = rel(path, scope);

    let mut out = String::new();
    let _ = writeln!(out, "# Conflicts: {} in {rel_path}", conflicts.len());

    for conflict in conflicts {
        let fn_label = conflict.enclosing_fn.as_deref().unwrap_or("<top level>");
        let cline = conflict.line;
        let _ = writeln!(out, "\n## {fn_label} (L{cline})");
        let _ = writeln!(out, "  OURS:");
        for l in conflict.ours.lines() {
            let _ = writeln!(out, "    {l}");
        }
        let _ = writeln!(out, "  THEIRS:");
        for l in conflict.theirs.lines() {
            let _ = writeln!(out, "    {l}");
        }
    }

    out
}

/// Format a git log range as a structured summary.
///
/// Layout:
/// ```text
/// # Log: range — N commits, M files, K functions touched
///
/// ## abc1234 — "message" (2h ago, @author)
///   path:  [~:sig] name, [+] name
/// ```
pub(crate) fn format_log(summaries: &[CommitSummary], scope: &str, budget: Option<u64>) -> String {
    let path_scope = Path::new(".");

    let total_files: usize = summaries.iter().map(|s| s.overlays.len()).sum();
    let total_fns: usize = summaries
        .iter()
        .flat_map(|s| s.overlays.iter())
        .flat_map(|o| o.symbol_changes.iter())
        .filter(|c| !matches!(c.change, ChangeType::Unchanged))
        .count();

    let n_commits = summaries.len();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Log: {scope} — {n_commits} {}, {total_files} {}, {total_fns} {} touched",
        noun(n_commits, "commit", "commits"),
        noun(total_files, "file", "files"),
        noun(total_fns, "function", "functions"),
    );

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    for summary in summaries {
        let short_hash = &summary.hash[..summary.hash.len().min(7)];
        let age = format_age(now - summary.timestamp);
        let _ = writeln!(
            out,
            "\n## {short_hash} — \"{}\" ({age}, @{})",
            summary.message, summary.author
        );

        for overlay in &summary.overlays {
            let rel_path = rel(&overlay.path, path_scope);
            let changed: Vec<&SymbolChange> = overlay
                .symbol_changes
                .iter()
                .filter(|c| !matches!(c.change, ChangeType::Unchanged))
                .collect();

            if changed.is_empty() {
                continue;
            }

            let markers: Vec<String> = changed
                .iter()
                .map(|c| {
                    format!(
                        "{} {}",
                        format_marker(&c.change, &c.match_confidence),
                        c.qualified_name()
                    )
                })
                .collect();
            let _ = writeln!(out, "  {rel_path}:  {}", markers.join(", "));
        }
    }

    if let Some(b) = budget {
        budget::apply(&out, b)
    } else {
        out
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Build the bracket marker for a symbol change.
/// `Ambiguous` confidence overrides to `[?→]` regardless of change type.
fn format_marker(change: &ChangeType, confidence: &MatchConfidence) -> String {
    if matches!(confidence, MatchConfidence::Ambiguous(_)) {
        return "[?→]".to_string();
    }
    match change {
        ChangeType::Added => "[+]".to_string(),
        ChangeType::Deleted => "[-]".to_string(),
        ChangeType::BodyChanged => "[~]".to_string(),
        ChangeType::SignatureChanged => "[~:sig]".to_string(),
        ChangeType::Renamed { .. } => "[→]".to_string(),
        ChangeType::Moved { .. } => "[↦]".to_string(),
        ChangeType::RenamedAndMoved { .. } => "[→↦]".to_string(),
        ChangeType::Unchanged => "[ ]".to_string(),
    }
}

/// Number of lines in a span.
fn span_len((start, end): (u32, u32)) -> u32 {
    end.saturating_sub(start) + 1
}

/// Format one symbol change as a single overview line (used inside file sections).
fn format_symbol_line(change: &SymbolChange) -> String {
    let marker = format_marker(&change.change, &change.match_confidence);
    let (line_no, _) = change.span();

    let mut s = format!("{marker:<8} {}", change.qualified_name());

    // A signature already holds the name, so it follows the name whole.
    if matches!(change.change, ChangeType::SignatureChanged) {
        if let (Some(old), Some(new)) = (&change.old_sig, &change.new_sig) {
            let _ = write!(s, " — {old} → {new}");
        }
    }

    // Line number at column ~50, at least two spaces after the text.
    let pad = 50usize.saturating_sub(s.chars().count()).max(2);
    let _ = write!(s, "{:>pad$}L{line_no}", "");

    // Trailing annotation.
    match &change.change {
        ChangeType::BodyChanged => {
            if let (Some(old), Some(new)) = (change.old_span, change.new_span) {
                let (old, new) = (span_len(old), span_len(new));
                let lines = noun(new, "line", "lines");
                let _ = write!(s, "  (body, {old}→{new} {lines})");
            }
        }
        ChangeType::Added => {
            if let Some(new) = change.new_span {
                let new = span_len(new);
                let _ = write!(s, "  (new, {new} {})", noun(new, "line", "lines"));
            }
        }
        ChangeType::Deleted => {
            if let Some(old) = change.old_span {
                let old = span_len(old);
                let _ = write!(s, "  (deleted, {old} {})", noun(old, "line", "lines"));
            }
        }
        _ => {}
    }

    s
}

/// Human-readable label for a change type.
fn change_type_label(change: &ChangeType) -> &'static str {
    match change {
        ChangeType::Added => "added",
        ChangeType::Deleted => "deleted",
        ChangeType::BodyChanged => "body changed",
        ChangeType::SignatureChanged => "signature changed",
        ChangeType::Renamed { .. } => "renamed",
        ChangeType::Moved { .. } => "moved",
        ChangeType::RenamedAndMoved { .. } => "renamed and moved",
        ChangeType::Unchanged => "unchanged",
    }
}

/// One changed symbol's section, under `name`: its heading, any signature
/// change, and its patch lines.
fn write_change(out: &mut String, name: &str, change: &SymbolChange) {
    let (line, end_line) = change.span();
    let marker = format_marker(&change.change, &change.match_confidence);
    let label = change_type_label(&change.change);
    let _ = writeln!(out, "\n## {marker} {name} — {label} (L{line}-{end_line})");
    write_signatures(out, change);
    write_diff_lines(out, &change.lines);
}

/// The sections of the first `n` changed symbols in overview order, each
/// named `path:symbol` as a scope names it. A symbol whose changed lines all
/// sit in smaller changed symbols, such as an `impl` around changed methods,
/// has nothing of its own to show and is passed over.
fn write_expanded(out: &mut String, overlays: &[FileOverlay], scope: &Path, n: usize) {
    let changes = overlays
        .iter()
        .flat_map(|o| o.symbol_changes.iter().map(move |c| (o, c)))
        .filter(|(_, c)| !matches!(c.change, ChangeType::Unchanged) && !c.lines.is_empty())
        .take(n);
    for (overlay, change) in changes {
        let name = format!("{}:{}", rel(&overlay.path, scope), change.qualified_name());
        write_change(out, &name, change);
    }
}

/// BEFORE and AFTER lines for a signature change.
fn write_signatures(out: &mut String, change: &SymbolChange) {
    if matches!(change.change, ChangeType::SignatureChanged) {
        if let Some(old_sig) = &change.old_sig {
            let _ = writeln!(out, "  BEFORE: {old_sig}");
        }
        if let Some(new_sig) = &change.new_sig {
            let _ = writeln!(out, "  AFTER:  {new_sig}");
        }
    }
}

/// Write diff lines with +/-/space prefixes and their file line numbers: the
/// old file's for a removed line, the new file's otherwise.
fn write_diff_lines(out: &mut String, lines: &[DiffLine]) {
    for dl in lines {
        let prefix = match dl.kind {
            DiffLineKind::Added => '+',
            DiffLineKind::Removed => '-',
            DiffLineKind::Context => ' ',
        };
        let _ = writeln!(out, "  {prefix}{:>4}| {}", dl.line, dl.content);
    }
}

fn write_warnings(out: &mut String, warnings: &[String]) {
    if !warnings.is_empty() {
        let _ = writeln!(out);
        for w in warnings {
            let _ = writeln!(out, "⚠ {w}");
        }
    }
}

/// Format a duration in seconds as a human-readable age string.
fn format_age(secs: i64) -> String {
    let secs = secs.max(0) as u64;
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    format!("{days}d ago")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{
        ChangeType, Conflict, DiffLine, DiffLineKind, FileOverlay, MatchConfidence, SymbolChange,
    };
    use std::path::PathBuf;
    use tilth_core::types::OutlineKind;

    fn make_overlay(path: &str, changes: Vec<SymbolChange>) -> FileOverlay {
        FileOverlay {
            path: PathBuf::from(path),
            old_path: None,
            symbol_changes: changes,
            other_lines: Vec::new(),
            added: 0,
            removed: 0,
            conflicts: Vec::new(),
            is_generated: false,
            is_binary: false,
        }
    }

    fn make_change(name: &str, change: ChangeType) -> SymbolChange {
        SymbolChange {
            name: name.to_string(),
            parent: String::new(),
            kind: OutlineKind::Function,
            change,
            match_confidence: MatchConfidence::Exact,
            old_span: Some((42, 51)),
            new_span: Some((42, 53)),
            old_sig: None,
            new_sig: None,
            lines: Vec::new(),
        }
    }

    fn make_sig_change(name: &str, old: &str, new: &str) -> SymbolChange {
        SymbolChange {
            name: name.to_string(),
            parent: String::new(),
            kind: OutlineKind::Function,
            change: ChangeType::SignatureChanged,
            match_confidence: MatchConfidence::Exact,
            old_span: Some((42, 51)),
            new_span: Some((42, 51)),
            old_sig: Some(old.to_string()),
            new_sig: Some(new.to_string()),
            lines: Vec::new(),
        }
    }

    // 1. All 6 change types produce correct markers
    #[test]
    fn test_overview_markers() {
        let changes = vec![
            make_change("fn_added", ChangeType::Added),
            make_change("fn_deleted", ChangeType::Deleted),
            make_change("fn_body", ChangeType::BodyChanged),
            make_sig_change("fn_sig", "old", "new"),
            make_change(
                "fn_renamed",
                ChangeType::Renamed {
                    old_name: "old_fn".to_string(),
                },
            ),
            make_change(
                "fn_moved",
                ChangeType::Moved {
                    old_path: PathBuf::from("old.rs"),
                },
            ),
        ];
        let overlay = make_overlay("src/lib.rs", changes);
        let out = format_overview(&[overlay], &[], "HEAD", 0, None);
        assert!(out.contains("[+]"), "missing [+]:\n{out}");
        assert!(out.contains("[-]"), "missing [-]:\n{out}");
        assert!(out.contains("[~]"), "missing [~]:\n{out}");
        assert!(out.contains("[~:sig]"), "missing [~:sig]:\n{out}");
        assert!(out.contains("[→]"), "missing [→]:\n{out}");
        assert!(out.contains("[↦]"), "missing [↦]:\n{out}");
    }

    // 2. Ambiguous confidence → [?→]
    #[test]
    fn test_overview_ambiguous_marker() {
        let mut change = make_change("fn_ambig", ChangeType::Added);
        change.match_confidence = MatchConfidence::Ambiguous(3);
        let overlay = make_overlay("src/lib.rs", vec![change]);
        let out = format_overview(&[overlay], &[], "HEAD", 0, None);
        assert!(out.contains("[?→]"), "expected [?→]:\n{out}");
    }

    // 3. Warnings appended at bottom
    #[test]
    fn test_overview_warnings() {
        let overlay = make_overlay("src/lib.rs", vec![]);
        let warnings = vec!["warning: foo changed in 2 locations".to_string()];
        let out = format_overview(&[overlay], &warnings, "HEAD", 0, None);
        assert!(
            out.contains("warning: foo changed"),
            "warnings not appended:\n{out}"
        );
        let warn_pos = out.find("warning: foo").unwrap();
        let file_pos = out.find("## src/lib.rs").unwrap();
        assert!(warn_pos > file_pos, "warning before file section:\n{out}");
    }

    // 4. Generated files show one-line summary
    #[test]
    fn test_overview_generated() {
        let mut overlay = make_overlay("package-lock.json", vec![]);
        // The counts come from the patch; a generated file has no symbols.
        overlay.added = 1;
        overlay.removed = 1;
        overlay.is_generated = true;
        let out = format_overview(&[overlay], &[], "HEAD", 0, None);
        assert!(out.contains("generated"), "missing 'generated':\n{out}");
        assert!(
            out.contains("lines changed"),
            "missing 'lines changed':\n{out}"
        );
        assert!(
            out.contains("2 lines changed"),
            "expected 2 changed lines:\n{out}"
        );
    }

    // 5. Binary files show "(binary)"
    #[test]
    fn test_overview_binary() {
        let mut overlay = make_overlay("image.png", vec![]);
        overlay.is_binary = true;
        let out = format_overview(&[overlay], &[], "HEAD", 0, None);
        assert!(out.contains("(binary)"), "missing (binary):\n{out}");
    }

    // 6. Small budget truncates with "... truncated"
    #[test]
    fn test_overview_budget() {
        let changes = vec![
            make_change("fn_a", ChangeType::Added),
            make_change("fn_b", ChangeType::BodyChanged),
            make_change("fn_c", ChangeType::Deleted),
        ];
        let overlay = make_overlay("src/lib.rs", changes);
        let out = format_overview(&[overlay], &[], "HEAD", 0, Some(5));
        assert!(out.contains("truncated"), "expected truncation:\n{out}");
    }

    // 7. Unchanged symbols NOT shown in overview
    #[test]
    fn test_overview_unchanged_hidden() {
        let changes = vec![
            make_change("fn_changed", ChangeType::BodyChanged),
            make_change("fn_unchanged", ChangeType::Unchanged),
        ];
        let overlay = make_overlay("src/lib.rs", changes);
        let out = format_overview(&[overlay], &[], "HEAD", 0, None);
        assert!(out.contains("fn_changed"), "changed fn missing:\n{out}");
        assert!(
            !out.contains("fn_unchanged"),
            "unchanged fn should be hidden:\n{out}"
        );
    }

    // 8. Multiple files — correct file count, both paths present
    #[test]
    fn test_overview_multiple_files() {
        let overlay_a = make_overlay("src/a.rs", vec![make_change("fn_a", ChangeType::Added)]);
        let overlay_b = make_overlay(
            "src/b.rs",
            vec![make_change("fn_b", ChangeType::BodyChanged)],
        );
        let out = format_overview(&[overlay_a, overlay_b], &[], "HEAD", 0, None);
        assert!(out.contains("2 files"), "wrong file count:\n{out}");
        assert!(out.contains("src/a.rs"), "missing a.rs:\n{out}");
        assert!(out.contains("src/b.rs"), "missing b.rs:\n{out}");
    }

    // 9. Signature annotation shows old→new for [~:sig]
    #[test]
    fn test_overview_signature_annotation() {
        let change = make_sig_change("process", "fn process(x: i32)", "fn process(x: i64)");
        let overlay = make_overlay("src/lib.rs", vec![change]);
        let out = format_overview(&[overlay], &[], "HEAD", 0, None);
        assert!(out.contains("[~:sig]"), "missing sig marker:\n{out}");
        assert!(
            out.contains("fn process(x: i32)"),
            "missing old sig:\n{out}"
        );
        assert!(
            out.contains("fn process(x: i64)"),
            "missing new sig:\n{out}"
        );
        assert!(out.contains('→'), "missing arrow:\n{out}");
    }

    fn line(kind: DiffLineKind, line: u32, content: &str) -> DiffLine {
        DiffLine {
            kind,
            line,
            content: content.to_string(),
        }
    }

    // 10. File detail header — symbol count and the patch's +N/-N
    #[test]
    fn test_file_detail_header() {
        let mut overlay = make_overlay(
            "src/lib.rs",
            vec![make_change("foo", ChangeType::BodyChanged)],
        );
        overlay.added = 2;
        overlay.removed = 1;
        let out = format_file_detail(&overlay, &[], None);
        assert!(
            out.starts_with("# Diff: src/lib.rs — 1 symbol touched, +2/\u{2212}1 lines"),
            "bad header:\n{out}"
        );
    }

    // 11. File detail — +/-/space prefixes with each line's own number
    #[test]
    fn test_file_detail_diff_lines() {
        let mut change = make_change("foo", ChangeType::BodyChanged);
        change.lines = vec![
            line(DiffLineKind::Removed, 44, "removed line"),
            line(DiffLineKind::Added, 44, "added line"),
            line(DiffLineKind::Context, 45, "context line"),
        ];
        let mut overlay = make_overlay("src/lib.rs", vec![change]);
        overlay.other_lines = vec![line(DiffLineKind::Added, 3, "use x;")];
        let out = format_file_detail(&overlay, &[], None);
        assert!(out.contains("  -  44| removed line"), "{out}");
        assert!(out.contains("  +  44| added line"), "{out}");
        assert!(out.contains("     45| context line"), "{out}");
        assert!(out.contains("## outside symbols\n  +   3| use x;"), "{out}");
    }

    // 12. File detail — BEFORE/AFTER labels for sig change
    #[test]
    fn test_file_detail_before_after() {
        let overlay = make_overlay(
            "src/lib.rs",
            vec![make_sig_change("foo", "foo(x: i32)", "foo(x: i64)")],
        );
        let out = format_file_detail(&overlay, &[], None);
        assert!(out.contains("BEFORE:"), "missing BEFORE:\n{out}");
        assert!(out.contains("AFTER:"), "missing AFTER:\n{out}");
        assert!(out.contains("foo(x: i32)"), "missing old sig:\n{out}");
        assert!(out.contains("foo(x: i64)"), "missing new sig:\n{out}");
    }

    // 13. File detail — [ ] marker for unchanged symbols
    #[test]
    fn test_file_detail_unchanged_context() {
        let changes = vec![
            make_change("fn_changed", ChangeType::BodyChanged),
            make_change("fn_unchanged", ChangeType::Unchanged),
        ];
        let overlay = make_overlay("src/lib.rs", changes);
        let out = format_file_detail(&overlay, &[], None);
        assert!(
            out.contains("[ ] fn_unchanged"),
            "missing unchanged marker:\n{out}"
        );
        assert!(out.contains("unchanged"), "missing unchanged label:\n{out}");
    }

    // 14. Function detail — correct name and content
    #[test]
    fn test_function_detail_found() {
        let mut change = make_change("my_fn", ChangeType::BodyChanged);
        change.lines = vec![line(DiffLineKind::Added, 43, "new code")];
        let overlay = make_overlay("src/lib.rs", vec![change]);
        let out = format_function_detail(&overlay, "my_fn");
        assert!(out.contains("my_fn"), "missing fn name:\n{out}");
        assert!(out.contains("new code"), "missing diff content:\n{out}");
        assert!(!out.contains("not found"), "unexpected 'not found':\n{out}");
    }

    // 14b. Function detail — a bare name two types share is ambiguous; the
    // qualified name, in either spelling, picks one.
    #[test]
    fn test_function_detail_qualified() {
        let mut a = make_change("run", ChangeType::BodyChanged);
        a.parent = "A".to_string();
        a.lines = vec![line(DiffLineKind::Added, 3, "in A")];
        let mut b = make_change("run", ChangeType::BodyChanged);
        b.parent = "B".to_string();
        b.lines = vec![line(DiffLineKind::Added, 9, "in B")];
        let overlay = make_overlay("src/s.ts", vec![a, b]);
        let out = format_function_detail(&overlay, "run");
        assert!(
            out.contains("ambiguous") && out.contains("A::run, B::run"),
            "{out}"
        );
        for name in ["B::run", "B.run"] {
            let out = format_function_detail(&overlay, name);
            assert!(
                out.contains("in B") && !out.contains("in A"),
                "{name}:\n{out}"
            );
        }
    }

    // 15. Function detail — "not found" message
    #[test]
    fn test_function_detail_not_found() {
        let overlay = make_overlay("src/lib.rs", vec![]);
        let out = format_function_detail(&overlay, "nonexistent");
        assert!(out.contains("not found"), "expected 'not found':\n{out}");
        assert!(out.contains("nonexistent"), "missing fn name:\n{out}");
    }

    // 16. Conflict format — ours/theirs with enclosing function
    #[test]
    fn test_conflict_format() {
        let conflicts = vec![Conflict {
            line: 42,
            ours: "let x = 1;".to_string(),
            theirs: "let x = 2;".to_string(),
            enclosing_fn: Some("compute".to_string()),
        }];
        let path = PathBuf::from("src/lib.rs");
        let out = format_conflicts(&conflicts, &path);
        assert!(out.starts_with("# Conflicts:"), "bad header:\n{out}");
        assert!(out.contains("compute"), "missing fn name:\n{out}");
        assert!(out.contains("L42"), "missing line number:\n{out}");
        assert!(out.contains("OURS:"), "missing OURS:\n{out}");
        assert!(out.contains("THEIRS:"), "missing THEIRS:\n{out}");
        assert!(out.contains("let x = 1;"), "missing ours content:\n{out}");
        assert!(out.contains("let x = 2;"), "missing theirs content:\n{out}");
    }

    // 17. Log format — per-commit with function markers
    #[test]
    fn test_log_format() {
        use crate::diff::CommitSummary;
        let overlay = make_overlay("src/lib.rs", vec![make_change("foo", ChangeType::Added)]);
        let summary = CommitSummary {
            hash: "abc1234def".to_string(),
            timestamp: 0,
            message: "add foo".to_string(),
            author: "alice".to_string(),
            overlays: vec![overlay],
        };
        let out = format_log(&[summary], "HEAD~1..HEAD", None);
        assert!(out.starts_with("# Log:"), "bad header:\n{out}");
        assert!(out.contains("abc1234"), "missing short hash:\n{out}");
        assert!(out.contains("add foo"), "missing commit message:\n{out}");
        assert!(out.contains("@alice"), "missing author:\n{out}");
        assert!(out.contains("[+]"), "missing marker:\n{out}");
        assert!(out.contains("foo"), "missing fn name:\n{out}");
    }
}
