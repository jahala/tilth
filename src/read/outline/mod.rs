pub mod code;
pub mod fallback;
pub mod markdown;
pub mod structured;
pub mod tabular;
pub mod test_file;

use std::path::Path;

use tilth_core::types::FileType;

const OUTLINE_CAP: usize = 100; // max outline lines for huge files

/// Generate a smart view based on file type.
pub fn generate(
    path: &Path,
    file_type: FileType,
    content: &str,
    buf: &[u8],
    capped: bool,
) -> String {
    let max_lines = if capped { OUTLINE_CAP } else { usize::MAX };

    // Test files get special treatment regardless of language
    if tilth_core::types::is_test_file(path) {
        if let FileType::Code(lang) = file_type {
            if let Some(outline) = test_file::outline(content, lang, max_lines) {
                return with_omission_note(outline, max_lines);
            }
        }
    }

    let outline = match file_type {
        FileType::Code(lang) => code::outline(content, lang, max_lines),
        FileType::Markdown => markdown::outline(buf, max_lines),
        FileType::StructuredData => structured::outline(path, content, max_lines),
        FileType::Tabular => tabular::outline(content, max_lines),
        FileType::Log => fallback::log_view(content),
        FileType::Other => fallback::head_tail(content),
    };
    with_omission_note(outline, max_lines)
}

/// The outline of a code file with names alone, no signatures or docs,
/// for a budget the full outline does not fit. `None` for any other file,
/// and for a test file, whose outline is its test structure.
pub fn names_only(path: &Path, file_type: FileType, content: &str, capped: bool) -> Option<String> {
    let FileType::Code(lang) = file_type else {
        return None;
    };
    if tilth_core::types::is_test_file(path) {
        return None;
    }
    let max_lines = if capped { OUTLINE_CAP } else { usize::MAX };
    let outline = code::outline_with(content, lang, max_lines, code::Detail::Names);
    Some(with_omission_note(outline, max_lines))
}

/// `outline` cut to fit in `room` tokens, with a closing line that says
/// what was left out. First the signatures and docs go, by switching to
/// `names` (the same outline with names alone) when there is one, so every
/// symbol stays listed; then whole entries from the end, counted. Cut at
/// the budget mid-entry, the outline hid its trailing symbols and said
/// only how many tokens it left out.
pub fn fit(outline: &str, names: Option<String>, room: u64) -> String {
    if crate::types::estimate_tokens(outline.len() as u64) <= room {
        return outline.to_string();
    }
    let compacted = names.is_some();
    let lines_text = names.unwrap_or_else(|| outline.to_string());
    // A view without `[n]` entries (head and tail, a log, test structure)
    // is counted in lines.
    let has_entries = lines_text
        .lines()
        .any(|l| l.trim_start().starts_with('['));
    let is_entry = |line: &str| !has_entries || line.trim_start().starts_with('[');
    let unit = if has_entries { "entries" } else { "lines" };
    let total = lines_text.lines().filter(|l| is_entry(l)).count();
    let note = |shown: usize| {
        let mut parts = Vec::new();
        if compacted {
            parts.push("signatures and docs left out".to_string());
        }
        if shown < total {
            parts.push(format!("{shown} of {total} {unit} shown"));
        }
        format!(
            "\n\n> outline cut to fit the budget: {}. Read a range with section, \
             or search a name.",
            parts.join("; ")
        )
    };
    let lines: Vec<&str> = lines_text.lines().collect();
    // Bytes and entries of the first k lines, so each cut is measured
    // without joining: the note's count changes with the cut.
    let mut bytes = vec![0usize];
    let mut entries = vec![0usize];
    for line in &lines {
        let joined = usize::from(bytes.len() != 1);
        bytes.push(bytes[bytes.len() - 1] + line.len() + joined);
        entries.push(entries[entries.len() - 1] + usize::from(is_entry(line)));
    }
    let keep = (0..=lines.len())
        .rev()
        .find(|&k| {
            crate::types::estimate_tokens((bytes[k] + note(entries[k]).len()) as u64) <= room
        })
        .unwrap_or(0);
    format!("{}{}", lines[..keep].join("\n"), note(entries[keep]))
}

/// Append a note when the outline likely hit `max_lines` and more symbols
/// exist below. Without this note, agents read the outline as exhaustive
/// and miss symbols below the cap.
///
/// Note: `max_lines` is an entry cap inside `format_entries` (one
/// `out.push(...)` per entry, joined with `\n`). For the code-outline path
/// `outline.lines().count() == entry_count` exactly. For other backends
/// (markdown, structured, tabular) the same identity holds because each
/// pushes single-line entries. So the heuristic compares like-for-like.
/// We avoid claiming a specific count in the user-facing message — we
/// only state that more symbols exist, which is the actionable signal.
fn with_omission_note(outline: String, max_lines: usize) -> String {
    if max_lines == usize::MAX {
        return outline;
    }
    if outline.lines().count() < max_lines {
        return outline;
    }
    format!(
        "{outline}\n\n> outline truncated — more symbols exist below the cap. \
         Use section=\"<start>-<end>\" with the line numbers shown in [...] \
         brackets above, or tilth_search \"<name>\" for a specific symbol."
    )
}

#[cfg(test)]
mod tests {
    use super::with_omission_note;
    use std::fmt::Write as _;

    /// A head-and-tail view, a log or a key list has no `[n]` entries, so
    /// the fit note said "outline cut to fit the budget: ." with nothing
    /// after it. It counts lines for such a view.
    #[test]
    fn fit_note_counts_lines_when_the_view_has_no_entries() {
        let view = (0..400).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "log line {i} with some text in it");
            s
        });
        let cut = super::fit(&view, None, 200);
        assert!(!cut.contains("budget: ."), "{cut}");
        let shown = cut.lines().filter(|l| l.starts_with("log line")).count();
        assert!(
            cut.contains(&format!("{shown} of 400 lines shown")),
            "{}",
            &cut[cut.len().saturating_sub(200)..]
        );
    }

    #[test]
    fn note_appended_when_at_cap() {
        let outline = (0..100)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = with_omission_note(outline, 100);
        assert!(result.contains("outline truncated"));
    }

    #[test]
    fn no_note_when_under_cap() {
        let outline = (0..50)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = with_omission_note(outline.clone(), 100);
        assert_eq!(result, outline);
    }

    #[test]
    fn no_note_when_uncapped() {
        let outline = (0..200)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = with_omission_note(outline.clone(), usize::MAX);
        assert_eq!(result, outline);
    }

    /// Integration test: drive the full `generate()` pipeline with a
    /// real Rust source containing more than `OUTLINE_CAP` top-level
    /// functions. Verifies the cap actually fires and that
    /// `with_omission_note` is wired into the pipeline correctly —
    /// not just exercised in isolation.
    #[test]
    fn integration_note_on_capped_code_file() {
        let src = (0..150).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "pub fn func_{i}() {{}}");
            s
        });
        let path = std::path::Path::new("fake.rs");
        let file_type = tilth_core::types::FileType::Code(tilth_core::types::Lang::Rust);
        let result = super::generate(path, file_type, &src, src.as_bytes(), true);
        assert!(
            result.contains("outline truncated"),
            "expected truncation note for 150 funcs over OUTLINE_CAP=100, got:\n{result}"
        );
    }

    /// Integration test: a small file (5 functions) must NOT produce
    /// the truncation note even when `capped=true` is passed, because
    /// the actual entry count is well below the cap.
    #[test]
    fn integration_no_note_on_small_code_file() {
        let src = (0..5).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "pub fn func_{i}() {{}}");
            s
        });
        let path = std::path::Path::new("fake.rs");
        let file_type = tilth_core::types::FileType::Code(tilth_core::types::Lang::Rust);
        let result = super::generate(path, file_type, &src, src.as_bytes(), true);
        assert!(
            !result.contains("outline truncated"),
            "spurious truncation note for 5 funcs (under OUTLINE_CAP=100):\n{result}"
        );
    }
}
