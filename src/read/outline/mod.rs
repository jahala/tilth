pub mod code;
pub mod fallback;
pub mod markdown;
pub mod structured;
pub mod tabular;
pub mod test_file;

use std::path::Path;

use crate::types::FileType;

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
    if crate::types::is_test_file(path) {
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

/// Append a note when the outline likely hit `max_lines` and more symbols
/// exist below. Without this note, agents read the outline as exhaustive
/// and miss symbols below the cap. Heuristic: line count >= cap signals
/// truncation; we accept the rare false positive on files with exactly
/// `OUTLINE_CAP` lines for the simpler implementation.
fn with_omission_note(outline: String, max_lines: usize) -> String {
    if max_lines == usize::MAX {
        return outline;
    }
    if outline.lines().count() < max_lines {
        return outline;
    }
    format!(
        "{outline}\n\n> outline truncated at {max_lines} lines — more symbols exist below. \
         Use section=\"start-end\" for a specific range, or tilth_search \"<name>\" for a known symbol."
    )
}

#[cfg(test)]
mod tests {
    use super::with_omission_note;

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
}
