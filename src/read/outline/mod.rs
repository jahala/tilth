pub mod code;
pub mod fallback;
pub mod markdown;
pub mod structured;
pub mod tabular;
pub mod test_file;

use std::path::Path;

use tilth_core::types::{FileType, Lang};

use crate::read::ends::{Ends, Shape};

const OUTLINE_CAP: usize = 100; // max outline lines for huge files

fn max_lines(capped: bool) -> usize {
    if capped {
        OUTLINE_CAP
    } else {
        usize::MAX
    }
}

/// Generate a smart view based on file type.
pub fn generate(
    path: &Path,
    file_type: FileType,
    content: &str,
    buf: &[u8],
    capped: bool,
) -> String {
    let max_lines = max_lines(capped);

    // Test files get special treatment regardless of language
    if tilth_core::types::is_test_file(path) {
        if let FileType::Code(lang) = file_type {
            if let Some(outline) = test_file::outline(content, lang, max_lines) {
                return with_omission_note(outline, max_lines);
            }
        }
    }

    let outline = match View::of(file_type) {
        View::Code(lang) => code::outline(content, lang, max_lines),
        View::Markdown => markdown::outline(buf, max_lines),
        View::Structured => structured::outline(path, content, max_lines),
        View::Ends(view) => view.outline_of_text(content),
    };
    with_omission_note(outline, max_lines)
}

/// What [`generate`] makes a file type's outline from: the whole text, or
/// only its first and last lines. The one mapping from file type to view,
/// so a text in memory and a file streamed from disk get the same view.
pub(crate) enum View {
    Code(Lang),
    Markdown,
    Structured,
    Ends(EndsView),
}

impl View {
    pub(crate) fn of(file_type: FileType) -> Self {
        match file_type {
            FileType::Code(lang) => Self::Code(lang),
            FileType::Markdown => Self::Markdown,
            FileType::StructuredData => Self::Structured,
            FileType::Log => Self::Ends(EndsView::Log),
            FileType::Tabular => Self::Ends(EndsView::Tabular),
            FileType::Other => Self::Ends(EndsView::Other),
        }
    }
}

/// A view that shows only a text's first and last lines and its line count,
/// so a file can stream into it without being held in memory.
#[derive(Clone, Copy)]
pub(crate) enum EndsView {
    Log,
    Tabular,
    Other,
}

impl EndsView {
    /// How many lines the view shows from each end.
    pub(crate) fn shape(self) -> Shape {
        match self {
            Self::Log => fallback::LOG,
            Self::Tabular => tabular::TABULAR,
            Self::Other => fallback::OTHER,
        }
    }

    /// What [`generate`] returns for a text whose [`shape`](Self::shape)
    /// ends are `ends`.
    pub(crate) fn generate(self, ends: &Ends<'_>, capped: bool) -> String {
        with_omission_note(self.render(ends), max_lines(capped))
    }

    /// The view of `text`, held in memory, before any omission note.
    fn outline_of_text(self, text: &str) -> String {
        self.render(&Ends::of_text(text, self.shape()))
    }

    fn render(self, ends: &Ends<'_>) -> String {
        match self {
            Self::Log | Self::Other => fallback::excerpt(ends),
            Self::Tabular => tabular::render(ends),
        }
    }
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
    let max_lines = max_lines(capped);
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
    let is_entry = |line: &str| line.trim_start().starts_with('[');
    let lines: Vec<&str> = lines_text.lines().collect();
    let total_entries = lines.iter().filter(|l| is_entry(l)).count();
    // What the first `kept` lines leave out, as entries when an entry goes
    // and as lines when only other lines (a view without `[n]` entries, a
    // closing note) do. Nothing left out and nothing compacted: no note.
    let note = |kept: usize, entries: usize| {
        let mut parts = Vec::new();
        if compacted {
            parts.push("signatures and docs left out".to_string());
        }
        if entries < total_entries {
            parts.push(format!("{entries} of {total_entries} entries shown"));
        } else if kept < lines.len() {
            parts.push(format!("{kept} of {} lines shown", lines.len()));
        }
        if parts.is_empty() {
            return String::new();
        }
        format!(
            "\n\n> outline cut to fit the budget: {}. Read a range with section, \
             or search a name.",
            parts.join("; ")
        )
    };
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
            crate::types::estimate_tokens((bytes[k] + note(k, entries[k]).len()) as u64) <= room
        })
        .unwrap_or(0);
    format!("{}{}", lines[..keep].join("\n"), note(keep, entries[keep]))
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

    /// When the cut dropped only lines that are not entries, such as the
    /// closing "outline truncated" note of a capped outline, every entry
    /// stayed and the note still said "outline cut to fit the budget: ."
    /// with nothing after it.
    #[test]
    fn fit_note_names_cut_lines_when_every_entry_stays() {
        let mut view = (0..20).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "[{i}] fn f{i}()");
            s
        });
        view.push_str(&"> a closing note that is not an entry. ".repeat(20));
        let cut = super::fit(&view, None, 120);
        assert!(!cut.contains("budget: ."), "{cut}");
        assert!(cut.contains("[19] fn f19()"), "{cut}");
        assert!(cut.contains("20 of 21 lines shown"), "{cut}");
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
