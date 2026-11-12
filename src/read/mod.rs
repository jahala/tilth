pub(crate) mod ends;
pub mod outline;

use std::fmt::Write;
use std::fs;
use std::io::{self, Read};
use std::ops::ControlFlow;
use std::path::Path;

use crate::format;
use crate::types::{estimate_tokens, ViewMode};
use ends::EndsCollector;
use outline::View;
use tilth_core::cache::OutlineCache;
use tilth_core::error::TilthError;
use tilth_core::lang::detection;
use tilth_core::lang::outline::{heading_level, heading_text, parse_markdown};
use tilth_core::lang::{detect_file_type, detect_file_type_with_content};
use tilth_core::source::{read_chunks, OpenFile, STREAM_CHUNK};
use tilth_core::source::{strip_bom, strip_bom_bytes};
use tilth_core::types::FileType;

pub(crate) const TOKEN_THRESHOLD: u64 = 6_000;
/// How many of a file's local imports the "Related:" hint under an outlined
/// read names, in import order.
const RELATED_HINT_LIMIT: usize = 8;
/// Minimum compression ratio required to prefer the outline over full content.
/// If `outline_tokens >= full_tokens * OUTLINE_MIN_COMPRESSION / 100`, the
/// outline saves too few tokens to be worth the information loss, so the full
/// file is returned instead. Integer arithmetic: 80 means 80%.
const OUTLINE_MIN_COMPRESSION: u64 = 80;

/// Lines in `buf`, counted the way `wc -l` does, plus a final line that has
/// no newline. A newline ends a line, so a newline-terminated file has as
/// many lines as newlines. Counting newlines + 1 made every such file one
/// line longer than it is, and let a section past the last line succeed
/// with an empty body.
pub(crate) fn count_lines(buf: &[u8]) -> u32 {
    let mut counter = LineCounter::default();
    counter.feed(buf);
    counter.lines()
}

/// [`count_lines`] over input that arrives in pieces: feed it the bytes in
/// order, then ask for [`lines`](Self::lines). One home for the rule, whether
/// the file is in memory or streamed.
#[derive(Default)]
pub(crate) struct LineCounter {
    newlines: usize,
    last: Option<u8>,
}

impl LineCounter {
    pub(crate) fn feed(&mut self, chunk: &[u8]) {
        self.feed_counted(chunk, memchr::memchr_iter(b'\n', chunk).count());
    }

    /// [`feed`](Self::feed) for a caller that has already counted the
    /// `newlines` in `piece`, so the piece is not scanned again.
    fn feed_counted(&mut self, piece: &[u8], newlines: usize) {
        self.newlines += newlines;
        if let Some(&last) = piece.last() {
            self.last = Some(last);
        }
    }

    /// The line breaks fed so far.
    fn newlines(&self) -> usize {
        self.newlines
    }

    pub(crate) fn lines(&self) -> u32 {
        let unterminated = usize::from(self.last.is_some_and(|b| b != b'\n'));
        u32::try_from(self.newlines + unterminated).unwrap_or(u32::MAX)
    }
}

/// Whether bytes fed in order, in pieces of any size, are UTF-8. A
/// character split between two pieces waits for the next one.
#[derive(Default)]
struct Utf8Check {
    /// The start of a character the last piece cut off: at most 3 bytes.
    pending: Vec<u8>,
    invalid: bool,
}

impl Utf8Check {
    /// Take the next piece. Breaks at the first byte that is not UTF-8.
    fn feed(&mut self, piece: &[u8]) -> ControlFlow<()> {
        let joined;
        let bytes = if self.pending.is_empty() {
            piece
        } else {
            self.pending.extend_from_slice(piece);
            joined = std::mem::take(&mut self.pending);
            &joined[..]
        };
        match std::str::from_utf8(bytes) {
            Ok(_) => ControlFlow::Continue(()),
            // The piece ends inside a character.
            Err(e) if e.error_len().is_none() => {
                self.pending.extend_from_slice(&bytes[e.valid_up_to()..]);
                ControlFlow::Continue(())
            }
            Err(_) => {
                self.invalid = true;
                ControlFlow::Break(())
            }
        }
    }

    /// Whether everything fed was UTF-8, the last character whole.
    fn is_valid(&self) -> bool {
        !self.invalid && self.pending.is_empty()
    }
}

/// Hand `head`, then the bytes `rest` yields, to `each` in order, until the
/// input ends or `each` breaks. This is the one streamed pass of the read
/// paths: `head` is what a caller already holds of the file, and the rest
/// streams through one buffer, so a pass that keeps only what it needs
/// never holds the file in memory.
fn stream(
    head: &[u8],
    rest: impl Read,
    mut each: impl FnMut(&[u8]) -> ControlFlow<()>,
) -> io::Result<()> {
    if each(head).is_break() {
        return Ok(());
    }
    let mut buf = vec![0u8; STREAM_CHUNK];
    read_chunks(rest, &mut buf, each)
}

/// The error a read reports for an I/O failure on `path`, so a read fails
/// the same way whichever step hit the error. A missing path names a
/// similar one.
fn path_error(path: &Path, source: io::Error) -> TilthError {
    TilthError::from_io(path, source, || suggest_similar(path))
}

/// Open `path` through [`OpenFile`], the one open path of every read, with
/// its errors mapped through [`path_error`]. `None` for a pipe, socket or
/// device, which has no fixed contents and shows as empty.
fn open_file(path: &Path) -> Result<Option<OpenFile>, TilthError> {
    OpenFile::open(path).map_err(|e| path_error(path, e))
}

/// The view of an empty file, and of a pipe, socket or device: the header
/// alone.
pub(crate) fn empty_view(path: &Path) -> String {
    format::file_header(path, 0, 0, ViewMode::Empty)
}

/// The whole text of `path` and its length, for a view that reshapes the
/// whole file, or `None` for a file with no fixed contents. It opens and
/// fails the way `read_file` does.
pub(crate) fn read_text(path: &Path) -> Result<Option<(String, u64)>, TilthError> {
    let Some(mut file) = open_file(path)? else {
        return Ok(None);
    };
    let text = file.read_to_string().map_err(|e| path_error(path, e))?;
    Ok(Some((text, file.len())))
}

/// The default view of a file that is not code, for the signature and
/// stripped views, which reshape only code. Both have always read the file
/// as UTF-8 text first, so a file that is not UTF-8 fails here as it always
/// has, with the error that read gave. It is checked one chunk at a time,
/// never held whole, and the default view then streams as it does in any
/// read.
pub(crate) fn non_code_view(path: &Path, cache: &OutlineCache) -> Result<String, TilthError> {
    let Some(mut file) = open_file(path)? else {
        return Ok(empty_view(path));
    };
    let mut utf8 = Utf8Check::default();
    file.reader()
        .and_then(|reader| stream(&[], reader, |chunk| utf8.feed(chunk)))
        .map_err(|e| path_error(path, e))?;
    if !utf8.is_valid() {
        let source = io::Error::new(
            io::ErrorKind::InvalidData,
            "stream did not contain valid UTF-8",
        );
        return Err(path_error(path, source));
    }
    read_file(path, None, None, cache, false, None)
}

/// The view of a file that `full` asked for in full but that is over `cap`:
/// its outline, under a banner that says how to lift the cap.
fn full_skipped_view(
    path: &Path,
    byte_len: u64,
    line_count: u32,
    cap: u64,
    outline: &str,
) -> String {
    #[expect(
        clippy::cast_precision_loss,
        reason = "by design: display only; file sizes fit the f64 mantissa"
    )]
    let cap_mb = cap as f64 / 1_000_000.0;
    #[expect(
        clippy::cast_precision_loss,
        reason = "by design: display only; file sizes fit the f64 mantissa"
    )]
    let file_mb = byte_len as f64 / 1_000_000.0;
    let header = format::file_header(path, byte_len, line_count, ViewMode::Outline);
    format!(
        "{header}\n\n> **full=true skipped**: file is {file_mb:.1}MB (cap: {cap_mb:.1}MB). \
         Use `section` to read specific ranges, or set TILTH_FULL_SIZE_CAP={byte_len} to override.\n\n{outline}"
    )
}

/// OGATE: whether `outline` is meaningfully smaller than the `tokens` of the
/// full file. If not, the full file is the better view: the same token cost
/// with less information is strictly worse and forces a second read.
fn outline_saves_enough(outline: &str, tokens: u64) -> bool {
    estimate_tokens(outline.len() as u64) < tokens * OUTLINE_MIN_COMPRESSION / 100
}

/// `outline` under `header`, cut to fit `budget` when it does not, falling
/// back first to `names`, the same outline with names alone.
fn outline_view(
    header: &str,
    outline: &str,
    names: impl FnOnce() -> Option<String>,
    budget: Option<u64>,
) -> String {
    let view = format!("{header}\n\n{outline}");
    match budget {
        Some(b) if estimate_tokens(view.len() as u64) > b => {
            let room = b.saturating_sub(estimate_tokens(header.len() as u64 + 2));
            format!("{header}\n\n{}", outline::fit(outline, names(), room))
        }
        _ => view,
    }
}

/// Main entry point for read mode. Routes through the decision tree.
///
/// `full_cap` asks for the whole file when it is at most that many bytes
/// ([`Config::full_size_cap`](crate::config::Config::full_size_cap)); a
/// file over the cap gets its outline under a banner instead. `None` lets
/// the smart view choose.
///
/// `budget` is the caller's token budget for the whole response. A file
/// whose full view does not fit it gets its outline instead, and an
/// outline that does not fit is cut to fit, saying what it left out. With
/// `full` or a `section`, the content is what was asked for, and the
/// caller's budget cut applies to it as before.
#[expect(
    clippy::too_many_lines,
    reason = "reading goes through one Source in #308"
)]
pub fn read_file(
    path: &Path,
    section: Option<&str>,
    full_cap: Option<u64>,
    cache: &OutlineCache,
    edit_mode: bool,
    budget: Option<u64>,
) -> Result<String, TilthError> {
    let full = full_cap.is_some();
    let meta = fs::metadata(path).map_err(|e| path_error(path, e))?;

    // Directory → list contents
    if meta.is_dir() {
        return list_directory(path);
    }

    // An empty file prints its header only, before its section or its
    // permissions are looked at. So does a pipe, socket or device, which
    // has no fixed contents.
    if !meta.is_file() || meta.len() == 0 {
        return Ok(empty_view(path));
    }

    // Section param → return those lines verbatim, any size
    if let Some(range) = section {
        return read_ranges(path, &[range], edit_mode);
    }

    // From here on, everything describes the file this handle reads: its
    // size, its emptiness and the mtime its outline is cached under, even if
    // a rename replaced the path after the metadata call above.
    let Some(mut file) = open_file(path)?.filter(|file| !file.is_empty()) else {
        return Ok(empty_view(path));
    };
    let byte_len = file.len();
    let mtime = file
        .metadata()
        .modified()
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

    // The binary, generated and minified checks look only at the head. A
    // file they settle costs that small read plus a streamed line count, so
    // a multi-gigabyte binary or bundle is never held in memory.
    let mut reader = file.reader().map_err(|e| path_error(path, e))?;
    let head = detection::read_head(&mut reader).map_err(|e| path_error(path, e))?;
    if detection::is_binary(&head) {
        let mime = mime_from_ext(path);
        return Ok(format::binary_header(path, byte_len, mime));
    }

    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    // Minified: the filename convention or, for big files, the newline
    // density of the head.
    let header_only =
        if detection::is_generated_by_name(name) || detection::is_generated_by_content(&head) {
            Some(ViewMode::Generated)
        } else if detection::is_minified_by_name(name)
            || (byte_len >= detection::MINIFIED_CHECK_THRESHOLD
                && detection::is_minified_by_content(&head))
        {
            Some(ViewMode::Minified)
        } else {
            None
        };
    if let Some(mode) = header_only {
        let mut counter = LineCounter::default();
        stream(&head, &mut reader, |chunk| {
            counter.feed(chunk);
            ControlFlow::Continue(())
        })
        .map_err(|e| path_error(path, e))?;
        return Ok(format::file_header(path, byte_len, counter.lines(), mode));
    }

    // A log, CSV or unknown-type file that gets its outline shows only its
    // first and last lines and its line count. It streams into them, so
    // however large it is, only the lines shown are held.
    let outlined = full_cap.map_or_else(
        || estimate_tokens(byte_len) > TOKEN_THRESHOLD,
        |cap| byte_len > cap,
    );
    let ends_view = match View::of(detect_file_type(path)) {
        View::Ends(view) if outlined => Some(view),
        _ => None,
    };
    // The whole file, when a streamed pass has found it holds no more lines
    // than its view shows: that pass then holds all of it already.
    let mut whole = None;
    if let Some(view) = ends_view {
        // The views see the text without its byte-order mark.
        let (mark, unmarked) = head.split_at(head.len() - strip_bom_bytes(&head).len());
        let mut collector = EndsCollector::new(view.shape());
        stream(unmarked, &mut reader, |chunk| {
            collector.feed(chunk);
            ControlFlow::Continue(())
        })
        .map_err(|e| path_error(path, e))?;
        match collector.finish().into_whole() {
            Ok(mut text) => {
                text.splice(..0, mark.iter().copied());
                whole = Some(text);
            }
            Err(ends) => {
                // The mark holds no line break, and this file has more lines
                // than the view shows, so the count without it is the file's.
                let line_count = u32::try_from(ends.total()).unwrap_or(u32::MAX);
                let capped = full || byte_len > tilth_core::source::MAX_FILE_SIZE;
                let outline = cache.get_or_compute(path, mtime, || view.generate(&ends, capped));
                if let Some(cap) = full_cap {
                    return Ok(full_skipped_view(path, byte_len, line_count, cap, &outline));
                }
                if outline_saves_enough(&outline, estimate_tokens(byte_len)) {
                    let header = format::file_header(path, byte_len, line_count, ViewMode::Outline);
                    return Ok(outline_view(&header, &outline, || None, budget));
                }
                // OGATE prefers the full view, which needs the lines these
                // ends left out: the file is read whole below, and the
                // outline comes from the cache.
            }
        }
    }

    let bytes = match whole {
        Some(bytes) => bytes,
        None => file.read_all().map_err(|e| path_error(path, e))?,
    };
    let byte_len = bytes.len() as u64;
    let buf = &bytes[..];

    let tokens = estimate_tokens(byte_len);
    let content = String::from_utf8_lossy(buf);
    let line_count = count_lines(buf);

    // Guard: full=true on very large files. Return outline + warning instead of
    // dumping megabytes that would blow up the MCP client's timeout/memory.
    if let Some(cap) = full_cap.filter(|&cap| byte_len > cap) {
        let file_type = detect_file_type_with_content(path, &content);
        let outline = cache.get_or_compute(path, mtime, || {
            outline::generate(
                path,
                file_type,
                strip_bom(&content),
                strip_bom_bytes(buf),
                true,
            )
        });
        return Ok(full_skipped_view(path, byte_len, line_count, cap, &outline));
    }

    // Canonical full-content view — shared by the small-file gate and OGATE below.
    let full_view = || {
        let header = format::file_header(path, byte_len, line_count, ViewMode::Full);
        if edit_mode {
            let numbered = format::hashlines(&content, 1);
            format!("{header}\n\n{numbered}")
        } else {
            format!("{header}\n\n{content}")
        }
    };

    // Full mode or small file → return full content (skip smart view),
    // unless the full view overflows the budget: cut there, it showed the
    // first functions and hid the rest.
    let fits = |view: &str| budget.is_none_or(|b| estimate_tokens(view.len() as u64) <= b);
    if full || tokens <= TOKEN_THRESHOLD {
        let view = full_view();
        if full || fits(&view) {
            return Ok(view);
        }
    }

    // Large file → smart view by file type
    let file_type = detect_file_type_with_content(path, &content);

    // Over the size limit search and the parse cache skip, the outline
    // lists at most `outline::OUTLINE_CAP` entries.
    let capped = byte_len > tilth_core::source::MAX_FILE_SIZE;

    // The outline parses the file without its byte-order mark, as search
    // does; the full view above prints the bytes as they are, because its
    // hash anchors must match what `tilth_write` reads from disk.
    let outline = cache.get_or_compute(path, mtime, || {
        outline::generate(
            path,
            file_type,
            strip_bom(&content),
            strip_bom_bytes(buf),
            capped,
        )
    });

    // OGATE, only when the full view fits the budget, though: cut to fit, it
    // would show the first lines and hide everything after them, where the
    // outline below names them all.
    if !outline_saves_enough(&outline, tokens) {
        let view = full_view();
        if fits(&view) {
            return Ok(view);
        }
    }

    let mode = match file_type {
        FileType::StructuredData => ViewMode::Keys,
        _ => ViewMode::Outline,
    };
    let header = format::file_header(path, byte_len, line_count, mode);
    let names = || outline::names_only(path, file_type, strip_bom(&content), capped);
    Ok(outline_view(&header, &outline, names, budget))
}

/// The "Related:" hint under an outlined read of a file whose local imports
/// are `related`, in import order: the first [`RELATED_HINT_LIMIT`], then
/// how many more there are. Empty when there are none.
pub(crate) fn related_hint<'a>(related: impl ExactSizeIterator<Item = &'a Path>) -> String {
    let total = related.len();
    if total == 0 {
        return String::new();
    }
    let mut hint = String::from("\n\n> Related: ");
    for (i, path) in related.take(RELATED_HINT_LIMIT).enumerate() {
        if i > 0 {
            hint.push_str(", ");
        }
        let _ = write!(hint, "{}", path.display());
    }
    if total > RELATED_HINT_LIMIT {
        let _ = write!(hint, ", and {} more", total - RELATED_HINT_LIMIT);
    }
    hint
}

/// Would this file produce an outline (rather than full content) in default read mode?
/// Used by the MCP layer to decide whether to append related-file hints.
pub fn would_outline(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| !m.is_dir() && estimate_tokens(m.len()) > TOKEN_THRESHOLD)
}

/// Resolve a heading address to a line range in a markdown file.
/// Returns `(start_line, end_line)` as 1-indexed inclusive range.
/// Returns `None` if heading not found.
///
/// Walks the `tree-sitter-md` `section` tree: each ATX heading owns a
/// `section` node spanning from the heading line through the line before
/// the next same-or-higher-level heading (sub-headings nest as child
/// sections and don't terminate the parent). Headings inside fenced /
/// indented code blocks aren't emitted as `atx_heading` nodes, so the
/// fence-state tracking the previous hand-rolled scanner needed is now
/// the parser's responsibility.
fn resolve_heading(buf: &[u8], heading: &str) -> Option<(usize, usize)> {
    let heading_trimmed = heading.trim_end();
    let query_level = heading_trimmed.chars().take_while(|&c| c == '#').count();
    if query_level == 0 || query_level > 6 {
        return None;
    }
    // Normalise the query the same way `heading_text` normalises an
    // `atx_heading` node — strip leading `#`s, surrounding whitespace,
    // and any ATX-close `#`s — so `## Foo`, `## Foo ##`, and `##  Foo`
    // all match the same node.
    let query_text = heading_trimmed[query_level..]
        .trim()
        .trim_end_matches('#')
        .trim();
    if query_text.is_empty() {
        return None;
    }

    let content = std::str::from_utf8(buf).ok()?;
    let tree = parse_markdown(content)?;
    let lines: Vec<&str> = content.lines().collect();

    let level = query_level as u8;
    find_section(tree.root_node(), &lines, level, query_text)
}

/// Recursive section-tree walk for `resolve_heading`. Returns the first
/// section whose `atx_heading` matches `(level, text)`.
fn find_section(
    node: tree_sitter::Node,
    lines: &[&str],
    target_level: u8,
    target_text: &str,
) -> Option<(usize, usize)> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "section" => {
                if let Some(hit) = match_section(child, lines, target_level, target_text) {
                    return Some(hit);
                }
                if let Some(hit) = find_section(child, lines, target_level, target_text) {
                    return Some(hit);
                }
            }
            // The parser owns these — no headings hide inside.
            "fenced_code_block" | "indented_code_block" | "html_block" => {}
            _ => {
                if let Some(hit) = find_section(child, lines, target_level, target_text) {
                    return Some(hit);
                }
            }
        }
    }
    None
}

fn match_section(
    section: tree_sitter::Node,
    lines: &[&str],
    target_level: u8,
    target_text: &str,
) -> Option<(usize, usize)> {
    let mut cursor = section.walk();
    let heading = section
        .children(&mut cursor)
        .find(|c| c.kind() == "atx_heading")?;
    if heading_level(heading) != Some(target_level) {
        return None;
    }
    if heading_text(heading, lines) != target_text {
        return None;
    }
    let start_line = heading.start_position().row + 1;
    let end_line = section_end_line(section);
    Some((start_line, end_line))
}

/// 1-indexed inclusive last line of a tree-sitter `section` node.
/// `end_position` is exclusive; col 0 means we landed on the next line's
/// row, so the section's last line is `end.row` itself.
fn section_end_line(section: tree_sitter::Node) -> usize {
    let end = section.end_position();
    if end.column == 0 {
        end.row
    } else {
        end.row + 1
    }
}

/// Return up to `top_n` markdown headings ranked by edit distance to `query`.
///
/// Used when a heading lookup misses — agents typo'd anchors, or the heading
/// renamed since they last read. Returning the closest matches lets them
/// retry with the right anchor without re-reading the whole file.
///
/// Walks `atx_heading` nodes from `tree-sitter-md`, which by construction
/// covers `CommonMark` §4.6 (1–6 `#`s followed by space/EOL) and excludes
/// headings inside fenced or indented code blocks. `Setext` headings
/// (`Title\n===`) are silently ignored — see `find_defs_markdown_buf` for
/// the same trade-off; the block grammar puts them at document scope so
/// span computation doesn't apply.
///
/// Caveat on ranking: Levenshtein favours candidates of similar length to
/// the query, so very short queries against long headings can rank tighter
/// matches first; this is acceptable for a hint and aligned with the rest
/// of the project's `edit_distance` use.
fn suggest_headings(buf: &[u8], query: &str, top_n: usize) -> Vec<String> {
    let q_text = query.trim_end().trim_start_matches('#').trim();
    if q_text.is_empty() {
        return Vec::new();
    }
    let q_lower = q_text.to_ascii_lowercase();

    let Ok(content) = std::str::from_utf8(buf) else {
        return Vec::new();
    };
    let Some(tree) = parse_markdown(content) else {
        return Vec::new();
    };
    let lines: Vec<&str> = content.lines().collect();

    let mut scored: Vec<(usize, String)> = Vec::new();
    collect_atx_headings(tree.root_node(), &lines, &q_lower, &mut scored);
    scored.sort_by_key(|(d, _)| *d);
    scored.into_iter().take(top_n).map(|(_, h)| h).collect()
}

/// Recursively collect `atx_heading` nodes scored by edit distance to
/// `q_lower`. Code blocks are skipped — the grammar already guarantees
/// no `atx_heading` nests inside them, but we elide the recursion to
/// avoid walking large fenced bodies.
fn collect_atx_headings(
    node: tree_sitter::Node,
    lines: &[&str],
    q_lower: &str,
    out: &mut Vec<(usize, String)>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "atx_heading" => {
                let h_text = heading_text(child, lines);
                // Strip kramdown attr blocks before scoring (e.g. `## Foo {#id}`).
                let h_clean = h_text.split('{').next().unwrap_or(&h_text).trim();
                if h_clean.is_empty() {
                    continue;
                }
                let dist = edit_distance(q_lower, &h_clean.to_ascii_lowercase());
                let row = child.start_position().row;
                let line_text = lines.get(row).copied().unwrap_or("").trim_end().to_string();
                out.push((dist, line_text));
            }
            "fenced_code_block" | "indented_code_block" | "html_block" => {}
            _ => collect_atx_headings(child, lines, q_lower, out),
        }
    }
}

/// Resolve a single range string (line range like "45-89" or heading like
/// "## Architecture") to a 1-indexed inclusive `(start, end)` pair.
fn resolve_range(buf: &[u8], range: &str) -> Result<(usize, usize), TilthError> {
    if range.starts_with('#') {
        resolve_heading(buf, range).ok_or_else(|| {
            let suggestions = suggest_headings(buf, range, 5);
            let reason = if suggestions.is_empty() {
                "heading not found in file".to_string()
            } else {
                format!(
                    "heading not found in file. Closest matches:\n  {}",
                    suggestions.join("\n  ")
                )
            };
            TilthError::InvalidQuery {
                query: range.to_string(),
                reason,
            }
        })
    } else {
        line_range(range)
    }
}

/// A `"start-end"` range, or the error that names the accepted forms.
fn line_range(range: &str) -> Result<(usize, usize), TilthError> {
    parse_range(range).ok_or_else(|| TilthError::InvalidQuery {
        query: range.to_string(),
        reason:
            "expected format: \"start-end\" (e.g. \"45-89\") or heading (e.g. \"## Architecture\")"
                .into(),
    })
}

/// One resolved range, ready to format.
struct Block {
    start: usize, // 1-indexed inclusive
    end: usize,   // 1-indexed inclusive (clamped to file length)
    text: String,
}

/// Read one or more line ranges from a file. Each range is "start-end"
/// (e.g. "45-89") or a heading anchor (e.g. "## Architecture") for
/// markdown files. Reads the file once and emits a single `[section]`
/// header followed by the formatted blocks; when more than one range is
/// requested, each block is preceded by a `─── lines X-Y ───` delimiter.
///
/// Ranges are emitted in the order supplied — overlapping or out-of-order
/// ranges are honored verbatim, not coalesced or sorted. Any invalid
/// range fails the whole call.
pub fn read_ranges(path: &Path, ranges: &[&str], edit_mode: bool) -> Result<String, TilthError> {
    let Some((blocks, total_bytes, total_lines)) = collect_blocks(path, ranges, edit_mode)? else {
        return Ok(empty_view(path));
    };
    let header = format::file_header(path, total_bytes, total_lines, ViewMode::Section);

    if blocks.len() == 1 {
        let b = &blocks[0];
        return Ok(format!("{header}\n\n{}", b.text));
    }

    let mut out = String::with_capacity(header.len() + total_bytes as usize + blocks.len() * 32);
    out.push_str(&header);
    for b in &blocks {
        let _ = write!(out, "\n\n─── lines {}-{} ───\n", b.start, b.end);
        out.push_str(&b.text);
    }
    Ok(out)
}

/// Like [`read_ranges`], but with a strict shared token budget across all
/// sections.
///
/// Blocks are emitted in user order. Once the remaining budget cannot hold a
/// later block, that block is replaced with an omission marker while the marker
/// itself still fits. If the first block alone exceeds the budget, the standard
/// truncator cuts that first block and all later blocks are omitted.
pub fn read_ranges_with_budget(
    path: &Path,
    ranges: &[&str],
    edit_mode: bool,
    budget: u64,
) -> Result<String, TilthError> {
    let Some((blocks, total_bytes, total_lines)) = collect_blocks(path, ranges, edit_mode)? else {
        return Ok(empty_view(path));
    };
    let header = format::file_header(path, total_bytes, total_lines, ViewMode::Section);

    if blocks.len() == 1 {
        let single = format!("{header}\n\n{}", blocks[0].text);
        return Ok(crate::budget::apply(&single, budget));
    }

    let mut out = String::with_capacity(header.len() + total_bytes as usize + blocks.len() * 32);
    out.push_str(&header);
    let mut remaining = budget.saturating_sub(estimate_tokens(header.len() as u64));

    for (i, b) in blocks.iter().enumerate() {
        let chunk = format!("\n\n─── lines {}-{} ───\n{}", b.start, b.end, b.text);
        let tokens = estimate_tokens(chunk.len() as u64);
        if tokens <= remaining {
            out.push_str(&chunk);
            remaining -= tokens;
            continue;
        }
        if i == 0 {
            let single = format!("{header}{chunk}");
            return Ok(crate::budget::apply(&single, budget));
        }
        let marker = format!(
            "\n\n─── lines {}-{} ───\n... section omitted (budget exhausted) ...",
            b.start, b.end
        );
        let marker_tokens = estimate_tokens(marker.len() as u64);
        if marker_tokens > remaining {
            break;
        }
        out.push_str(&marker);
        remaining -= marker_tokens;
    }
    Ok(out)
}

/// The formatted blocks of a section read, with the bytes and lines they
/// hold, or `None` for an empty file and for a pipe, socket or device,
/// which has no fixed contents: both show as empty, whatever their
/// well-formed ranges.
fn collect_blocks(
    path: &Path,
    ranges: &[&str],
    edit_mode: bool,
) -> Result<Option<(Vec<Block>, u64, u32)>, TilthError> {
    if ranges.is_empty() {
        return Err(TilthError::InvalidQuery {
            query: String::new(),
            reason: "at least one range is required".into(),
        });
    }
    let Some(mut file) = open_file(path)?.filter(|file| !file.is_empty()) else {
        // Nothing to select from, so every range shows as empty, but a
        // malformed one fails here as it does on any file.
        for range in ranges.iter().filter(|range| !range.starts_with('#')) {
            line_range(range)?;
        }
        return Ok(None);
    };

    // A heading anchor needs the text to find its lines. Line ranges do not,
    // so without one the file streams and is never held in memory.
    let text = if ranges.iter().any(|range| range.starts_with('#')) {
        Some(file.read_all().map_err(|e| path_error(path, e))?)
    } else {
        None
    };
    let resolved: Vec<Result<(usize, usize), TilthError>> = ranges
        .iter()
        .map(|range| {
            text.as_ref()
                .map_or_else(|| line_range(range), |text| resolve_range(text, range))
        })
        .collect();

    let mut selector = LineSelector::new(&resolved);
    match &text {
        Some(text) => stream(text, io::empty(), |chunk| selector.feed(chunk)),
        None => file
            .reader()
            .and_then(|reader| stream(&[], reader, |chunk| selector.feed(chunk))),
    }
    .map_err(|e| path_error(path, e))?;
    let (selected, total) = selector.finish();

    let mut blocks: Vec<Block> = Vec::with_capacity(ranges.len());
    let mut total_bytes: u64 = 0;
    let mut total_lines: u32 = 0;
    for ((range, resolved), selected) in ranges.iter().zip(resolved).zip(selected) {
        let (start, end) = resolved?;
        if start > total {
            return Err(TilthError::InvalidQuery {
                query: (*range).to_string(),
                reason: format!(
                    "starts past the end of the file, which has {total} {}",
                    format::noun(total, "line", "lines")
                ),
            });
        }
        // An end past the last line clamps to it.
        let end = end.min(total);
        let text = String::from_utf8_lossy(&selected);
        total_bytes += text.len() as u64;
        total_lines += (end - (start - 1)) as u32;
        let formatted = if edit_mode {
            format::hashlines(&text, start as u32)
        } else {
            format::number_lines(&text, start as u32)
        };
        blocks.push(Block {
            start,
            end,
            text: formatted,
        });
    }
    Ok(Some((blocks, total_bytes, total_lines)))
}

/// Slices the requested line ranges out of a file fed to it in order, in
/// pieces of any size: the whole text at once, or streamed chunks. Line
/// breaks are found with `memchr`, once each, and the ranges are looked at
/// only where one starts or ends: a stretch of lines between two such
/// points is copied at once to the ranges open over it, or, when none is,
/// only counted. Feeding stops once the last requested line is in. Each
/// range is a valid `(start, end)` or the error that rejected it; only
/// valid ranges select.
struct LineSelector {
    /// The valid ranges no line has reached yet, as `(start, end, index)`,
    /// latest start first, so the next to open is the last.
    waiting: Vec<(usize, usize, usize)>,
    /// The ranges the current line is in, as `(end, index)`.
    open: Vec<(usize, usize)>,
    /// The last line any valid range asks for; 0 when none is valid.
    last: usize,
    /// The selected bytes, in the caller's range order.
    selected: Vec<Vec<u8>>,
    /// Counts the bytes taken so far, by the one line-count rule. The line
    /// the next byte belongs to is one past the breaks it has seen.
    counter: LineCounter,
    stopped: bool,
}

impl LineSelector {
    fn new(ranges: &[Result<(usize, usize), TilthError>]) -> Self {
        let mut waiting: Vec<(usize, usize, usize)> = ranges
            .iter()
            .enumerate()
            .filter_map(|(index, range)| range.as_ref().ok().map(|&(s, e)| (s, e, index)))
            .collect();
        waiting.sort_unstable_by_key(|&(start, ..)| std::cmp::Reverse(start));
        let last = waiting.iter().map(|&(_, end, _)| end).max().unwrap_or(0);
        Self {
            waiting,
            open: Vec::new(),
            last,
            selected: vec![Vec::new(); ranges.len()],
            counter: LineCounter::default(),
            stopped: last == 0,
        }
    }

    /// Take the next piece of the file. Breaks once the last requested line
    /// is in, or at once when no range is valid.
    fn feed(&mut self, chunk: &[u8]) -> ControlFlow<()> {
        if self.stopped {
            return ControlFlow::Break(());
        }
        let mut breaks = memchr::memchr_iter(b'\n', chunk);
        let mut from = 0;
        loop {
            let line = self.counter.newlines() + 1;
            self.open.retain(|&(end, _)| end >= line);
            while let Some(&(start, end, index)) = self.waiting.last() {
                if start > line {
                    break;
                }
                self.open.push((end, index));
                self.waiting.pop();
            }
            if line > self.last {
                self.stopped = true;
                return ControlFlow::Break(());
            }
            // The next line at which a range opens or closes. A range may
            // end at `usize::MAX`, and no line reaches that.
            let next = self
                .open
                .iter()
                .map(|&(end, _)| end.saturating_add(1))
                .chain(self.waiting.last().map(|&(start, ..)| start))
                .min()
                .unwrap_or_else(|| self.last.saturating_add(1));
            let mut crossed = 0;
            let mut to = None;
            for at in breaks.by_ref() {
                crossed += 1;
                if line + crossed == next {
                    to = Some(at + 1);
                    break;
                }
            }
            let stretch = &chunk[from..to.unwrap_or(chunk.len())];
            for &(_, index) in &self.open {
                self.selected[index].extend_from_slice(stretch);
            }
            self.counter.feed_counted(stretch, crossed);
            match to {
                Some(to) => from = to,
                None => return ControlFlow::Continue(()),
            }
        }
    }

    /// The selected bytes per range, and the line count of what was taken.
    /// That is the file's own count when the whole file was fed. When
    /// feeding stopped early it is the last requested line, which is all
    /// any range can need, since every valid range starts and ends at or
    /// before it.
    fn finish(self) -> (Vec<Vec<u8>>, usize) {
        (self.selected, self.counter.lines() as usize)
    }
}

/// Parse "45-89" into (45, 89). 1-indexed.
fn parse_range(s: &str) -> Option<(usize, usize)> {
    let (a, b) = s.split_once('-')?;
    let start: usize = a.trim().parse().ok()?;
    let end: usize = b.trim().parse().ok()?;
    if start == 0 || end < start {
        return None;
    }
    Some((start, end))
}

/// List directory contents — treat as glob on dir/*.
fn list_directory(path: &Path) -> Result<String, TilthError> {
    let mut entries: Vec<String> = Vec::new();
    let read_dir = fs::read_dir(path).map_err(|e| TilthError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;

    let mut items: Vec<_> = read_dir.filter_map(std::result::Result::ok).collect();
    items.sort_by_key(std::fs::DirEntry::file_name);

    for entry in &items {
        let ft = entry.file_type().ok();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let meta = entry.metadata().ok();

        let suffix = match ft {
            Some(t) if t.is_dir() => "/".to_string(),
            Some(t) if t.is_symlink() => " →".to_string(),
            _ => meta.map_or_else(String::new, |m| {
                let tokens = estimate_tokens(m.len());
                format!("  ({tokens} {})", format::noun(tokens, "token", "tokens"))
            }),
        };
        entries.push(format!("  {name}{suffix}"));
    }

    let header = format!(
        "# {} ({} {})",
        path.display(),
        items.len(),
        format::noun(items.len(), "item", "items")
    );
    Ok(format!("{header}\n\n{}", entries.join("\n")))
}

/// Public entry point for did-you-mean on path-like fallthrough queries.
/// Resolves the query relative to scope and checks the parent directory.
pub fn suggest_similar_file(scope: &Path, query: &str) -> Option<String> {
    let resolved = scope.join(query);
    suggest_similar(&resolved)
}

/// Suggest a similar file name from the parent directory (edit distance).
fn suggest_similar(path: &Path) -> Option<String> {
    let parent = path.parent()?;
    let name = path.file_name()?.to_str()?;
    let entries = fs::read_dir(parent).ok()?;

    let mut best: Option<(usize, String)> = None;
    for entry in entries.flatten() {
        let candidate = entry.file_name();
        let candidate = candidate.to_string_lossy();
        let dist = edit_distance(name, &candidate);
        if dist <= 3 {
            match &best {
                Some((d, _)) if dist < *d => best = Some((dist, candidate.into_owned())),
                None => best = Some((dist, candidate.into_owned())),
                _ => {}
            }
        }
    }
    best.map(|(_, name)| name)
}

/// Levenshtein distance over Unicode scalar values.
///
/// Wraps `strsim::levenshtein`, which iterates `.chars()` so a single CJK
/// or emoji glyph counts as one edit unit (not 3-4 bytes). Used by both
/// filename suggestion (`suggest_similar`) and heading suggestion
/// (`suggest_headings`).
fn edit_distance(a: &str, b: &str) -> usize {
    strsim::levenshtein(a, b)
}

/// Guess MIME type from extension for binary file headers.
pub(crate) fn mime_from_ext(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("pdf") => "application/pdf",
        Some("zip") => "application/zip",
        Some("gz" | "tgz") => "application/gzip",
        Some("tar") => "application/x-tar",
        Some("wasm") => "application/wasm",
        Some("woff" | "woff2") => "font/woff2",
        Some("ttf" | "otf") => "font/ttf",
        Some("mp3") => "audio/mpeg",
        Some("mp4") => "video/mp4",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full-read cap a run gets by default.
    fn full_cap() -> u64 {
        crate::config::Config::default().full_size_cap
    }

    #[test]
    fn heading_found() {
        let input = b"# Title\nSome content\n## Section\nSection content\n";
        let result = resolve_heading(input, "## Section");

        assert_eq!(result, Some((3, 4)));
    }

    #[test]
    fn heading_not_found() {
        let input = b"# Title\nContent\n";
        let result = resolve_heading(input, "## Missing");

        assert_eq!(result, None);
    }

    #[test]
    fn heading_in_code_block() {
        let input = b"# Real\n```\n## Fake\n```\n";
        let result = resolve_heading(input, "## Fake");

        // Heading inside code block should be skipped
        assert_eq!(result, None);
    }

    #[test]
    fn duplicate_headings() {
        let input = b"## First\ntext\n## First\ntext\n";
        let result = resolve_heading(input, "## First");

        // Should return the first occurrence
        assert_eq!(result, Some((1, 2)));
    }

    #[test]
    fn last_heading_to_eof() {
        let input = b"# Start\ntext\n## End\nfinal line\n";
        let result = resolve_heading(input, "## End");

        // Last heading should extend to total_lines (4)
        assert_eq!(result, Some((3, 4)));
    }

    #[test]
    fn nested_sections() {
        let input = b"## A\ncontent\n### B\nmore\n## C\ntext\n";
        let result = resolve_heading(input, "## A");

        // ## A should include ### B, ending when ## C starts (line 5)
        // So range is [1, 4]
        assert_eq!(result, Some((1, 4)));
    }

    #[test]
    fn no_hashes() {
        let input = b"# Heading\ntext\n";

        // Empty string
        assert_eq!(resolve_heading(input, ""), None);

        // String without hashes
        assert_eq!(resolve_heading(input, "hello"), None);
    }

    /// Over its cap, `full=true` gives the outline and a warning. The cap is
    /// a parameter, so the file only has to be bigger than a small one.
    #[test]
    fn full_true_size_cap_returns_outline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.rs");
        let src = "pub fn func_0() { println!(\"hello\"); }\n\npub fn func_1() {}\n";
        std::fs::write(&path, src).unwrap();
        let cap = 40;
        assert!(src.len() > cap);

        let cache = OutlineCache::new();
        let result = read_file(&path, None, Some(cap as u64), &cache, false, None).unwrap();

        assert!(
            result.contains("full=true skipped"),
            "expected size cap warning, got: {result}"
        );
        assert!(
            result.contains("func_0") && result.contains("func_1"),
            "expected outline content in output: {result}"
        );
        let under = read_file(&path, None, Some(src.len() as u64), &cache, false, None).unwrap();
        assert!(
            under.contains(src.trim_end()) && !under.contains("full=true skipped"),
            "a file at its cap reads in full: {under}"
        );
    }

    #[test]
    fn suggest_headings_returns_close_matches() {
        let input = b"# Architecture\nfoo\n## Getting Started\nbar\n## Configuration\nbaz\n";
        let suggestions = suggest_headings(input, "## Get Started", 5);
        assert!(
            suggestions.iter().any(|h| h.contains("Getting Started")),
            "expected 'Getting Started' in suggestions, got: {suggestions:?}"
        );
    }

    #[test]
    fn suggest_headings_top_n_orders_by_distance() {
        let input = b"# A\nfoo\n## Configuration\nbar\n## Authentication\nbaz\n## Settings\nqux\n";
        // Whole-word typo of "Configuration" — Levenshtein favours close-length
        // candidates here, so "Configuration" must rank ahead of the others.
        let suggestions = suggest_headings(input, "## Configurashun", 5);
        assert!(
            suggestions[0].contains("Configuration"),
            "expected 'Configuration' first, got: {suggestions:?}"
        );
    }

    #[test]
    fn suggest_headings_skips_code_blocks() {
        let input = b"## Real Heading\nfoo\n```md\n## Inside Code\n```\n";
        let suggestions = suggest_headings(input, "## Heading", 5);
        // Heading inside code block must NOT appear
        assert!(
            !suggestions.iter().any(|h| h.contains("Inside Code")),
            "fenced heading leaked into suggestions: {suggestions:?}"
        );
        assert!(
            suggestions.iter().any(|h| h.contains("Real Heading")),
            "expected real heading in suggestions: {suggestions:?}"
        );
    }

    #[test]
    fn suggest_headings_empty_query_returns_empty() {
        let input = b"# A\n## B\n";
        assert_eq!(suggest_headings(input, "", 5), Vec::<String>::new());
        assert_eq!(suggest_headings(input, "###", 5), Vec::<String>::new());
    }

    /// `CommonMark` allows `~~~` as a fence delimiter. Headings inside
    /// must not be treated as suggestable.
    #[test]
    fn suggest_headings_skips_tilde_fenced_blocks() {
        let input = b"## Real Heading\nfoo\n~~~md\n## Inside Tilde Fence\n~~~\n";
        let suggestions = suggest_headings(input, "## Heading", 5);
        assert!(
            !suggestions.iter().any(|h| h.contains("Inside Tilde Fence")),
            "tilde-fenced heading leaked: {suggestions:?}"
        );
        assert!(
            suggestions.iter().any(|h| h.contains("Real Heading")),
            "real heading missing: {suggestions:?}"
        );
    }

    /// `CommonMark` §4.6.1 limits ATX headings to 1–6 `#`s. Lines with 7+
    /// hashes are not headings, even with a space after.
    #[test]
    fn suggest_headings_rejects_seven_or_more_hashes() {
        let input = b"## Real Heading\nfoo\n####### Not a Heading\n";
        let suggestions = suggest_headings(input, "## Heading", 5);
        assert!(
            !suggestions.iter().any(|h| h.contains("Not a Heading")),
            "7-hash line leaked as heading: {suggestions:?}"
        );
    }

    /// `CommonMark` §4.6.1: hashes must be followed by a space (or EOL).
    /// `##foo` (no space) is not a heading.
    #[test]
    fn suggest_headings_rejects_hashes_without_space() {
        let input = b"## Real Heading\nfoo\n##NoSpace\n";
        let suggestions = suggest_headings(input, "## Heading", 5);
        assert!(
            !suggestions.iter().any(|h| h.contains("NoSpace")),
            "##NoSpace leaked as heading: {suggestions:?}"
        );
    }

    /// Filename and heading suggestion rely on Unicode-scalar-level edit
    /// distance, not byte-level — locks in the contract `strsim::levenshtein`
    /// provides via its char-iterating wrapper. If `strsim` ever switches
    /// to a byte-level distance, this test fails loudly.
    #[test]
    fn edit_distance_is_unicode_aware() {
        // 设置 (Settings) and 設定 (Configuration) — different chars,
        // each one Unicode scalar. Distance should be 2, not 6.
        assert_eq!(edit_distance("设置", "設定"), 2);
        // emoji single-scalar: 🦀 vs 🐙 = distance 1.
        assert_eq!(edit_distance("🦀", "🐙"), 1);
        // ASCII baseline still works.
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    fn write_temp(name: &str, content: &str) -> std::path::PathBuf {
        use std::io::Write;
        let path = std::env::temp_dir().join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn read_ranges_single_matches_legacy_section() {
        // One range produces no `─── lines X-Y ───` delimiter — output is
        // identical in shape to the pre-multi-section read.
        let path = write_temp(
            "tilth_test_ranges_single.txt",
            "alpha\nbeta\ngamma\ndelta\nepsilon\n",
        );
        let out = read_ranges(&path, &["2-3"], false).unwrap();
        assert!(out.contains("[section]"), "expected section header: {out}");
        assert!(
            !out.contains("─── lines"),
            "single range must not emit delimiter: {out}"
        );
        assert!(out.contains("2  beta"), "expected line 2: {out}");
        assert!(out.contains("3  gamma"), "expected line 3: {out}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_ranges_disjoint_two_blocks() {
        let path = write_temp(
            "tilth_test_ranges_disjoint.txt",
            "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n",
        );
        let out = read_ranges(&path, &["1-2", "6-7"], false).unwrap();
        assert!(out.contains("─── lines 1-2 ───"), "first delimiter: {out}");
        assert!(out.contains("─── lines 6-7 ───"), "second delimiter: {out}");
        assert!(
            out.contains("1  l1") && out.contains("7  l7"),
            "content: {out}"
        );
        // Header reports summed lines — 2 + 2 = 4
        assert!(out.contains("(4 lines"), "summed line_count: {out}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_ranges_preserves_user_order() {
        // Out-of-order ranges are NOT sorted — emit verbatim.
        let path = write_temp("tilth_test_ranges_order.txt", "a\nb\nc\nd\ne\nf\n");
        let out = read_ranges(&path, &["5-6", "1-2"], false).unwrap();
        let later = out.find("─── lines 5-6 ───").unwrap();
        let earlier = out.find("─── lines 1-2 ───").unwrap();
        assert!(
            later < earlier,
            "5-6 must appear before 1-2 (user order): {out}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_ranges_overlap_is_emitted_verbatim() {
        // Overlap is honored — duplicated content, no coalescing.
        let path = write_temp("tilth_test_ranges_overlap.txt", "x1\nx2\nx3\nx4\nx5\n");
        let out = read_ranges(&path, &["1-3", "2-4"], false).unwrap();
        assert!(out.contains("─── lines 1-3 ───"), "first block: {out}");
        assert!(out.contains("─── lines 2-4 ───"), "second block: {out}");
        // line 2 ("x2") appears in both blocks
        let occurrences = out.matches("  x2\n").count();
        assert_eq!(
            occurrences, 2,
            "expected x2 to appear twice (overlap): {out}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_ranges_mixed_line_and_heading() {
        let path = write_temp(
            "tilth_test_ranges_mixed.md",
            "# Top\nintro line\n## Foo\nfoo body\n## Bar\nbar body\n",
        );
        let out = read_ranges(&path, &["1-2", "## Bar"], false).unwrap();
        assert!(
            out.contains("─── lines 1-2 ───"),
            "line-range delimiter: {out}"
        );
        // "## Bar" lives at lines 5-6 in this fixture
        assert!(
            out.contains("─── lines 5-6 ───"),
            "heading-resolved delimiter: {out}"
        );
        assert!(
            out.contains("intro line") && out.contains("bar body"),
            "content: {out}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_ranges_invalid_second_range_fails_whole_call() {
        let path = write_temp("tilth_test_ranges_invalid.txt", "one\ntwo\nthree\n");
        let err = read_ranges(&path, &["1-2", "not-a-range"], false).unwrap_err();
        assert!(
            matches!(err, TilthError::InvalidQuery { .. }),
            "expected InvalidQuery, got: {err:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_ranges_empty_input_errors() {
        let path = write_temp("tilth_test_ranges_empty.txt", "a\nb\n");
        let err = read_ranges(&path, &[], false).unwrap_err();
        assert!(
            matches!(err, TilthError::InvalidQuery { .. }),
            "expected InvalidQuery for empty input, got: {err:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_ranges_edit_mode_emits_hashlines_per_block() {
        // Edit-mode output must be hashlined inside every block, not just the first.
        // Hashline format is `<line>:<3hex>|<content>`.
        let path = write_temp(
            "tilth_test_ranges_edit_mode.txt",
            "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\n",
        );
        let out = read_ranges(&path, &["1-2", "5-6"], true).unwrap();
        // Both delimiters present
        assert!(out.contains("─── lines 1-2 ───"), "first delimiter: {out}");
        assert!(out.contains("─── lines 5-6 ───"), "second delimiter: {out}");
        // Hashline anchors present for at least one line in each block (the
        // exact hash depends on FNV-1a so just check the `<n>:<hex>|` shape).
        let line_one_match = out
            .lines()
            .any(|l| l.starts_with("1:") && l.contains("|alpha"));
        let line_six_match = out
            .lines()
            .any(|l| l.starts_with("6:") && l.contains("|zeta"));
        assert!(line_one_match, "expected hashline for line 1: {out}");
        assert!(line_six_match, "expected hashline for line 6: {out}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn budgeted_passes_through_when_budget_ample() {
        let path = write_temp(
            "tilth_test_budgeted_passthrough.txt",
            "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\n",
        );
        let plain = read_ranges(&path, &["1-2", "5-6"], false).unwrap();
        let budgeted = read_ranges_with_budget(&path, &["1-2", "5-6"], false, 100_000).unwrap();
        assert_eq!(plain, budgeted);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn budgeted_omits_later_sections_when_exhausted() {
        use std::fmt::Write as _;
        let big_line = "x".repeat(200);
        let body = (0..30).fold(String::new(), |mut s, _| {
            let _ = writeln!(s, "{big_line}");
            s
        });
        let path = write_temp("tilth_test_budgeted_omit.txt", &body);
        let budget = 240u64;
        let out =
            read_ranges_with_budget(&path, &["1-3", "10-12", "20-22"], false, budget).unwrap();

        assert!(out.contains("─── lines 1-3 ───"), "first marker: {out}");
        assert!(
            out.contains("─── lines 10-12 ───\n... section omitted (budget exhausted) ..."),
            "second omitted: {out}"
        );
        assert!(
            out.contains("─── lines 20-22 ───\n... section omitted (budget exhausted) ..."),
            "third omitted: {out}"
        );
        assert!(
            estimate_tokens(out.len() as u64) <= budget,
            "output {} tokens > budget {budget}: {out}",
            estimate_tokens(out.len() as u64)
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn budgeted_truncates_first_section_when_too_large() {
        use std::fmt::Write as _;
        let body = (0..200).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "line-{i}-padding-padding-padding");
            s
        });
        let path = write_temp("tilth_test_budgeted_first_too_large.txt", &body);
        let budget = 100u64;
        let out = read_ranges_with_budget(&path, &["1-150"], false, budget).unwrap();
        assert!(
            out.contains("... truncated"),
            "expected truncation sentinel: {out}"
        );
        assert!(
            estimate_tokens(out.len() as u64) <= budget,
            "output {} tokens > budget {budget}: {out}",
            estimate_tokens(out.len() as u64)
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn budgeted_preserves_user_order_until_exhaustion() {
        use std::fmt::Write as _;
        let big_line = "y".repeat(200);
        let body = (0..30).fold(String::new(), |mut s, _| {
            let _ = writeln!(s, "{big_line}");
            s
        });
        let path = write_temp("tilth_test_budgeted_order.txt", &body);
        let budget = 240u64;
        let out =
            read_ranges_with_budget(&path, &["20-22", "1-3", "10-12"], false, budget).unwrap();
        let pos_first = out.find("─── lines 20-22 ───").expect("first marker");
        let pos_second = out.find("─── lines 1-3 ───").expect("second marker");
        let pos_third = out.find("─── lines 10-12 ───").expect("third marker");
        assert!(
            pos_first < pos_second && pos_second < pos_third,
            "order: {out}"
        );
        assert!(
            out.contains("─── lines 10-12 ───\n... section omitted (budget exhausted) ..."),
            "third must be omitted: {out}"
        );
        assert!(
            estimate_tokens(out.len() as u64) <= budget,
            "output {} tokens > budget {budget}: {out}",
            estimate_tokens(out.len() as u64)
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn budgeted_never_exceeds_budget_even_when_markers_do_not_fit() {
        use std::fmt::Write as _;
        let big_line = "z".repeat(50);
        let body = (0..30).fold(String::new(), |mut s, _| {
            let _ = writeln!(s, "{big_line}");
            s
        });
        let path = write_temp("tilth_test_budgeted_strict_cap.txt", &body);
        let budget = 80u64;
        let out =
            read_ranges_with_budget(&path, &["1-3", "10-12", "20-22"], false, budget).unwrap();
        assert!(
            estimate_tokens(out.len() as u64) <= budget,
            "output {} tokens > budget {budget}: {out}",
            estimate_tokens(out.len() as u64)
        );
        let _ = std::fs::remove_file(&path);
    }

    // OGATE: "never-worse outline gate" — if the outline barely compresses
    // (outline_tokens >= 80% of full-file tokens), return full content instead.
    #[test]
    fn ogate_flat_file_over_the_budget_gets_its_outline_cut_to_fit() {
        use std::fmt::Write as _;
        // The same flat file as the OGATE test below, read under a budget its
        // full view cannot fit. Falling back to the full view and cutting it
        // there showed the first few functions and hid the rest. The outline
        // path keeps every name it can and says what it left out.
        let mut src = String::new();
        for i in 0..2000 {
            writeln!(src, "pub fn flat_{i}() {{}}").unwrap();
        }
        let path = write_temp("tilth_ogate_flat_budget.rs", &src);

        let cache = OutlineCache::new();
        let budget = 3_000;
        let result = read_file(&path, None, None, &cache, false, Some(budget)).unwrap();

        assert!(
            !result.contains("[full]"),
            "an over-budget flat file must not fall back to a cut full view: {}",
            result.lines().next().unwrap_or("")
        );
        assert!(
            result.contains("[outline]") && result.contains("of 2000 entries shown"),
            "the outline must say how many of the functions it names: {result}"
        );
        assert!(
            result.contains("fn flat_100"),
            "the outline must name far more than the first few functions"
        );
        assert!(
            estimate_tokens(result.len() as u64) <= budget,
            "output must fit the budget"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ogate_flat_file_returns_full_when_outline_barely_compresses() {
        use std::fmt::Write as _;
        // A file of empty-body one-liner fns. The signature IS essentially the
        // whole source line (only ` {}` is dropped), so any signature-bearing
        // outline is ≥80% of the full-file tokens and OGATE must fire. This holds
        // regardless of the outline's per-entry formatting overhead, so the test
        // does not silently break if that format is ever tightened. Enough lines
        // to clear TOKEN_THRESHOLD (6k) — otherwise the small-file gate returns
        // full before OGATE is even consulted, passing for the wrong reason.
        let mut src = String::new();
        for i in 0..2000 {
            writeln!(src, "pub fn flat_{i}() {{}}").unwrap();
        }
        // ~2000 * ~21 bytes ≈ 43 KB ≈ 10.7k tokens (> TOKEN_THRESHOLD = 6k).
        // Assert it so the test provably exercises OGATE rather than the
        // small-file gate (which would also return `[full]`).
        assert!(
            estimate_tokens(src.len() as u64) > TOKEN_THRESHOLD,
            "fixture must exceed TOKEN_THRESHOLD so OGATE, not the small-file gate, returns full"
        );
        let path = write_temp("tilth_ogate_flat.rs", &src);

        let cache = OutlineCache::new();
        let result = read_file(&path, None, None, &cache, false, None).unwrap();

        // Gate fired → full view. The header tag records the decision directly
        // (an outline would read `[outline]`), and the trailing fn's body braces
        // `() {}` only appear in verbatim full content — an outline renders the
        // signature without the body.
        assert!(
            result.contains("[full]"),
            "OGATE must return the full view; header was: {}",
            result.lines().next().unwrap_or("")
        );
        assert!(
            result.contains("flat_1999() {}"),
            "expected verbatim full content (last fn body present)"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Contrasting test: a large file with big function bodies compresses well
    /// and must still return the OUTLINE (gate must NOT fire).
    #[test]
    fn ogate_large_compressible_file_returns_outline() {
        use std::fmt::Write as _;
        // Each function has a large body (many comment lines) so the outline
        // (which only shows the `fn` signature) is a tiny fraction of the
        // full-file token count.
        let mut src = String::new();
        for i in 0..40 {
            writeln!(src, "pub fn heavy_{i}() {{").unwrap();
            // 200 lines of body per function → ~200 * 30 = 6 000 bytes each
            for j in 0..200 {
                writeln!(
                    src,
                    "    let _x_{j}: u64 = {j}_{i}_padding_value_that_fills_space;"
                )
                .unwrap();
            }
            writeln!(src, "}}").unwrap();
        }
        // Total ≈ 40 * 202 lines * ~50 bytes = ~400 KB → well over TOKEN_THRESHOLD
        let path = write_temp("tilth_ogate_heavy.rs", &src);

        let cache = OutlineCache::new();
        let result = read_file(&path, None, None, &cache, false, None).unwrap();

        // Gate must NOT have fired: the output should be an outline, not full content.
        // An outline shows signatures but omits the body.
        assert!(
            result.contains("heavy_0"),
            "expected outline to mention function name heavy_0: {}",
            &result[..result.len().min(400)]
        );
        // Body variable lines are only in the full file, not in an outline
        assert!(
            !result.contains("_x_0: u64"),
            "outline must not contain body variables; got full content instead"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// A newline ends a line; it does not start one. `gin/errors.go` has 175
    /// lines by `wc -l` and the header said 176. Full, outline, generated and
    /// section headers all count the same way.
    #[test]
    fn header_counts_lines_like_wc() {
        let dir = tempfile::tempdir().unwrap();
        let cache = OutlineCache::new();
        for (content, lines) in [
            ("one\n", "1 line"),
            ("one\ntwo\n", "2 lines"),
            ("one\ntwo", "2 lines"),
            ("one\n\n", "2 lines"),
            ("\n", "1 line"),
        ] {
            let path = dir.path().join("f.txt");
            std::fs::write(&path, content).unwrap();
            let full = read_file(&path, None, Some(full_cap()), &cache, false, None).unwrap();
            assert!(
                full.contains(&format!("({lines},")),
                "{content:?} should read as {lines}: {full}"
            );
            let all = read_ranges(&path, &["1-1000"], false).unwrap();
            assert!(
                all.contains(&format!("({lines},")),
                "{content:?} section should cover {lines}: {all}"
            );
        }
    }

    /// Run a [`LineSelector`] over `reader` the way `collect_blocks` streams.
    fn select(
        reader: impl Read,
        ranges: &[Result<(usize, usize), TilthError>],
    ) -> (Vec<Vec<u8>>, usize) {
        let mut selector = LineSelector::new(ranges);
        let mut buf = vec![0u8; STREAM_CHUNK];
        read_chunks(reader, &mut buf, |chunk| selector.feed(chunk)).unwrap();
        selector.finish()
    }

    /// The range read stops once its last requested line is in. The reader
    /// fails if it is read again after its one chunk, so a result at all
    /// proves the rest of the file was never read.
    #[test]
    fn a_range_read_stops_after_the_last_requested_line() {
        use tilth_core::test_readers::ThenFail;
        let (selected, total) = select(ThenFail::new(b"a\nb\nc\nd\n"), &[Ok((2, 3))]);
        assert_eq!(selected[0], b"b\nc\n");
        assert_eq!(total, 3);
    }

    /// Lines that arrive split across reads are sliced the same as from one
    /// buffer, and the count matches `count_lines`, a final line with no
    /// newline included.
    #[test]
    fn a_range_read_is_the_same_one_byte_at_a_time() {
        use tilth_core::test_readers::OneByte;
        let text = b"one\ntwo\n\nfour\nfive";
        let ranges = [Ok((2, 4)), Ok((5, 9)), Ok((1, 1))];
        let mut whole = LineSelector::new(&ranges);
        let _ = whole.feed(text);
        let (whole, whole_total) = whole.finish();
        let (split, split_total) = select(OneByte(text), &ranges);
        assert_eq!(whole, split);
        assert_eq!(whole_total, split_total);
        assert_eq!(split_total, count_lines(text) as usize);
        assert_eq!(split[0], b"two\n\nfour\n");
        assert_eq!(split[1], b"five");
        assert_eq!(split[2], b"one\n");
    }

    /// The spec for [`LineSelector`]: lines `start..=end` of `text`, sliced
    /// by line offsets the way the mapped read did.
    fn sliced(text: &[u8], start: usize, end: usize) -> Vec<u8> {
        let mut offsets = vec![0];
        offsets.extend(memchr::memchr_iter(b'\n', text).map(|at| at + 1));
        let total = count_lines(text) as usize;
        let end = end.min(total);
        let to = offsets.get(end).copied().unwrap_or(text.len());
        text[offsets[start - 1]..to].to_vec()
    }

    /// Ranges in any order, overlapping, repeated, one line long, running
    /// past the end, select what slicing by line offsets selects, in the
    /// caller's order, whatever the size of the pieces the file arrives in.
    #[test]
    fn many_ranges_select_what_slicing_selects() {
        let text = b"one\ntwo\r\n\nfour\nfive\nsix\nseven\neight\nnine\nten";
        let total = count_lines(text) as usize;
        let ranges = [
            (7, 9),
            (1, 3),
            (2, 2),
            (1, 3),
            (9, 40),
            (4, 7),
            (10, 10),
            (3, 8),
        ];
        let wrapped: Vec<Result<(usize, usize), TilthError>> =
            ranges.iter().map(|&range| Ok(range)).collect();
        for piece in [1, 2, 3, 7, text.len()] {
            let mut selector = LineSelector::new(&wrapped);
            for chunk in text.chunks(piece) {
                if selector.feed(chunk).is_break() {
                    break;
                }
            }
            let (selected, count) = selector.finish();
            assert_eq!(count, total, "piece {piece}");
            for (&(start, end), got) in ranges.iter().zip(&selected) {
                assert_eq!(
                    got,
                    &sliced(text, start, end),
                    "{start}-{end}, piece {piece}"
                );
            }
        }
    }

    /// The streamed UTF-8 check agrees with checking the whole text, for
    /// characters split across pieces in every way.
    #[test]
    fn a_streamed_utf8_check_agrees_with_checking_the_whole() {
        let texts: [&[u8]; 11] = [
            b"",
            b"plain",
            "\u{feff}é漢🦀\n".as_bytes(),
            b"\xff",
            b"ok \xe2\x82",
            b"\xe2\x82\xac euro",
            b"cut \xc3",
            b"\xc3( bad",
            b"\xed\xa0\x80 surrogate",
            b"\xf4\x90\x80\x80 too large",
            b"\xf0\x9f\xa6\x80\xf0\x9f",
        ];
        for text in texts {
            for piece in [1, 2, 3, 5, text.len().max(1)] {
                let mut check = Utf8Check::default();
                for chunk in text.chunks(piece) {
                    if check.feed(chunk).is_break() {
                        break;
                    }
                }
                assert_eq!(
                    check.is_valid(),
                    std::str::from_utf8(text).is_ok(),
                    "{text:?}, piece {piece}"
                );
            }
        }
    }

    /// A range may end at the largest number there is. Its end clamps to
    /// the last line, as any end past it does, and the next range still
    /// selects its own lines.
    #[test]
    fn a_range_ending_at_the_largest_number_clamps_to_the_last_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("twenty.txt");
        let mut text = String::new();
        for i in 1..=20 {
            writeln!(text, "line {i}").unwrap();
        }
        std::fs::write(&path, &text).unwrap();
        let out = read_ranges(&path, &["1-18446744073709551615", "5-6"], false).unwrap();
        let header = format::file_header(&path, (text.len() + 14) as u64, 22, ViewMode::Section);
        let expected = format!(
            "{header}\n\n─── lines 1-20 ───\n{}\n\n─── lines 5-6 ───\n{}",
            format::number_lines(&text, 1),
            format::number_lines("line 5\nline 6\n", 5),
        );
        assert_eq!(out, expected);
    }

    /// A section read of an empty file shows it as empty, whatever its
    /// well-formed ranges, as a single `section` of it always did.
    #[test]
    fn a_section_read_of_an_empty_file_shows_it_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.md");
        std::fs::write(&path, "").unwrap();
        let shapes: [&[&str]; 4] = [
            &["1-2"],
            &["1-2", "4-5"],
            &["## Heading"],
            &["## Heading", "3-4"],
        ];
        for ranges in shapes {
            assert_eq!(
                read_ranges(&path, ranges, false).unwrap(),
                empty_view(&path),
                "{ranges:?}"
            );
            assert_eq!(
                read_ranges_with_budget(&path, ranges, false, 100).unwrap(),
                empty_view(&path),
                "{ranges:?}"
            );
        }
        // A single `section` of an empty file shows it as empty before
        // looking at the range, malformed or not.
        let cache = OutlineCache::new();
        for range in ["1-2", "x-y"] {
            let single = read_file(&path, Some(range), None, &cache, false, None).unwrap();
            assert_eq!(single, empty_view(&path), "{range}");
        }
    }

    /// A malformed line range fails a section read of an empty file, of a
    /// pipe and of a device, as it fails one of any other file: the first
    /// malformed range, with the forms the query accepts.
    #[cfg(unix)]
    #[test]
    fn a_malformed_range_fails_even_where_there_is_nothing_to_select() {
        use tilth_core::test_readers::{make_pipe, unless_blocked_on};
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty.md");
        std::fs::write(&empty, "").unwrap();
        let pipe = dir.path().join("pipe");
        make_pipe(&pipe);
        let expected = "invalid query \"abc\": expected format: \"start-end\" \
                        (e.g. \"45-89\") or heading (e.g. \"## Architecture\")";
        let shapes: [&[&str]; 4] = [
            &["abc"],
            &["abc", "0-5"],
            &["1-2", "abc"],
            &["## Heading", "abc", "5-4"],
        ];
        for path in [empty.as_path(), pipe.as_path(), Path::new("/dev/null")] {
            for ranges in shapes {
                let errors = unless_blocked_on(&pipe, || {
                    [
                        read_ranges(path, ranges, false),
                        read_ranges_with_budget(path, ranges, false, 100),
                    ]
                    .map(|read| read.unwrap_err().to_string())
                })
                .expect("a read blocked on the pipe");
                assert_eq!(errors, [expected, expected], "{path:?} {ranges:?}");
            }
        }
    }

    /// A pipe, socket or device has no fixed contents. It shows as empty, as
    /// its zero length always made it, and it is never read: a pipe with no
    /// writer used to block the read forever.
    #[cfg(unix)]
    #[test]
    fn a_pipe_or_device_shows_as_empty() {
        use tilth_core::test_readers::{make_pipe, unless_blocked_on};
        let dir = tempfile::tempdir().unwrap();
        let pipe = dir.path().join("pipe");
        make_pipe(&pipe);
        let null = Path::new("/dev/null");

        for path in [pipe.as_path(), null] {
            let reads = unless_blocked_on(&pipe, || {
                let cache = OutlineCache::new();
                [
                    read_file(path, None, None, &cache, false, None),
                    read_file(path, Some("1-2"), None, &cache, false, None),
                    read_ranges(path, &["1-2", "4-5"], false),
                    read_ranges_with_budget(path, &["1-2", "4-5"], false, 100),
                ]
                .map(Result::unwrap)
            })
            .expect("a read blocked on the pipe");
            for out in reads {
                assert_eq!(out, empty_view(path));
            }
        }
        // The open itself does not block, so nothing can swap a pipe in
        // between a look at the path and the open.
        let opened = unless_blocked_on(&pipe, || open_file(&pipe).unwrap().is_none());
        assert_eq!(opened, Some(true));
    }

    /// A log, CSV or unknown-type file past the token threshold streams into
    /// its first and last lines. The view must be the one the whole text
    /// gives, byte-order mark, `\r\n` and missing final newline included, in
    /// the default read and in a `full` read over the cap.
    #[test]
    fn streamed_views_equal_the_whole_text_view() {
        let dir = tempfile::tempdir().unwrap();
        let mut row = 0;
        let mut text = |lines: usize, eol: &str| {
            let mut out = String::new();
            for _ in 0..lines {
                row += 1;
                write!(
                    out,
                    "{row},2026-11-11T12:00:00,value é {}{eol}",
                    "x".repeat(row % 50)
                )
                .unwrap();
            }
            out
        };
        let mut cases = Vec::new();
        for name in ["app.log", "table.csv", "notes.unknownext"] {
            cases.push((name, text(2_000, "\n"), false));
            cases.push((name, format!("\u{feff}{}", text(2_000, "\r\n")), false));
            let mut unterminated = text(2_000, "\n");
            unterminated.pop();
            cases.push((name, unterminated, false));
            cases.push((name, text(40_000, "\n"), true));
        }
        for (name, content, full) in cases {
            let path = dir.path().join(name);
            std::fs::write(&path, &content).unwrap();
            assert!(estimate_tokens(content.len() as u64) > TOKEN_THRESHOLD);
            assert_eq!(full, content.len() as u64 > full_cap());
            let cache = OutlineCache::new();
            let out =
                read_file(&path, None, full.then_some(full_cap()), &cache, false, None).unwrap();

            let file_type = detect_file_type(&path);
            assert!(matches!(View::of(file_type), View::Ends(_)), "{name}");
            let capped = full || content.len() as u64 > tilth_core::source::MAX_FILE_SIZE;
            let text = strip_bom(&content);
            let outline = outline::generate(&path, file_type, text, text.as_bytes(), capped);
            let lines = count_lines(content.as_bytes());
            let len = content.len() as u64;
            let expected = if full {
                full_skipped_view(&path, len, lines, full_cap(), &outline)
            } else {
                let header = format::file_header(&path, len, lines, ViewMode::Outline);
                format!("{header}\n\n{outline}")
            };
            assert_eq!(out, expected, "{name}, full {full}");
        }
    }

    /// A log with no more lines than its view shows is its own view, and a
    /// file whose outline saves too little gets the full view. The first is
    /// built from the lines the streamed pass already holds, the second
    /// from a second read. Both must be the views of the file's own bytes,
    /// byte-order mark and invalid UTF-8 included, in the default read, in
    /// edit mode and under a budget the full view does not fit.
    #[test]
    fn a_streamed_view_that_needs_the_whole_text_gets_it() {
        let dir = tempfile::tempdir().unwrap();
        let line = |width: usize| format!("{}\n", "y".repeat(width)).into_bytes();
        let mut marked = b"\xef\xbb\xbf".to_vec();
        marked.extend(line(3_000).repeat(12));
        let mut invalid = line(3_000).repeat(11);
        invalid.extend(b"bad \xff byte, no final newline");
        // Under the minified check's size, which would skip a file this wide.
        let cases = [
            ("few.log", line(3_000).repeat(12)),
            ("marked.log", marked),
            ("invalid.log", invalid),
            ("wide.unknownext", line(900).repeat(70)),
        ];
        for (name, bytes) in cases {
            let path = dir.path().join(name);
            std::fs::write(&path, &bytes).unwrap();
            let len = bytes.len() as u64;
            assert!(estimate_tokens(len) > TOKEN_THRESHOLD);
            let content = String::from_utf8_lossy(&bytes);
            let lines = count_lines(&bytes);
            let full_header = format::file_header(&path, len, lines, ViewMode::Full);
            let cache = OutlineCache::new();

            let out = read_file(&path, None, None, &cache, false, None).unwrap();
            assert_eq!(out, format!("{full_header}\n\n{content}"), "{name}");

            let out = read_file(&path, None, None, &cache, true, None).unwrap();
            let numbered = format::hashlines(&content, 1);
            assert_eq!(out, format!("{full_header}\n\n{numbered}"), "{name}, edit");

            let budget = 2_000;
            let out = read_file(&path, None, None, &cache, false, Some(budget)).unwrap();
            let text = strip_bom(&content);
            let file_type = detect_file_type(&path);
            let outline = outline::generate(&path, file_type, text, text.as_bytes(), false);
            let header = format::file_header(&path, len, lines, ViewMode::Outline);
            let expected = outline_view(&header, &outline, || None, Some(budget));
            assert_eq!(out, expected, "{name}, budget");
        }
    }

    /// Line ranges are read by streaming the file. The slices must be the
    /// file's own lines, in the order asked, overlapping ranges included,
    /// with a final line that has no newline kept as it is.
    #[test]
    fn streamed_line_ranges_return_the_files_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("five.txt");
        std::fs::write(&path, "one\ntwo\nthree\nfour\nfive").unwrap();

        let out = read_ranges(&path, &["4-9", "2-3", "3-4"], false).unwrap();
        let bodies: Vec<&str> = out.split("─── lines").skip(1).map(str::trim_end).collect();
        assert_eq!(bodies.len(), 3, "{out}");
        assert!(
            bodies[0].contains("4  four") && bodies[0].contains("5  five"),
            "{out}"
        );
        assert!(
            bodies[1].contains("2  two") && bodies[1].contains("3  three"),
            "{out}"
        );
        assert!(
            bodies[2].contains("3  three") && bodies[2].contains("4  four"),
            "{out}"
        );
        assert!(
            !bodies[1].contains("four"),
            "a range keeps only its own lines: {out}"
        );

        let err = read_ranges(&path, &["2-3", "x-y"], false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("x-y") && err.contains("start-end"), "{err}");
        let err = read_ranges(&path, &["6-7", "x-y"], false)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("6-7") && err.contains("5 lines"),
            "the first bad range is named: {err}"
        );
    }

    /// A file shown by its header alone (generated or minified) gets its line
    /// count from a streamed pass, which must agree with `count_lines`.
    #[test]
    fn a_header_only_view_counts_lines_like_count_lines() {
        let dir = tempfile::tempdir().unwrap();
        let cache = OutlineCache::new();
        let mut minified = "x".repeat(150_000);
        minified.push_str("\nend");
        for (name, content) in [
            ("package-lock.json", "{\n  \"a\": 1\n}\n".to_string()),
            ("bundle.js", minified),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, &content).unwrap();
            let out = read_file(&path, None, None, &cache, false, None).unwrap();
            let lines = count_lines(content.as_bytes());
            assert!(out.contains(&format!("{lines} lines")), "{name}: {out}");
        }
    }

    /// A section that starts past the last line used to succeed with an empty
    /// body. It is an error that names the file's length.
    #[test]
    fn section_past_the_end_is_an_error_naming_the_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("two.txt");
        std::fs::write(&path, "alpha\nbeta\n").unwrap();
        let err = read_ranges(&path, &["3-100"], false)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("3-100") && err.contains("2 lines"),
            "error must name the range and the length: {err}"
        );
        // An end past EOF still clamps to the last line.
        let out = read_ranges(&path, &["2-100"], false).unwrap();
        assert!(out.contains("(1 line,") && out.contains("2  beta"), "{out}");
    }

    /// The CSV outline counted the final newline as a row too.
    #[test]
    fn tabular_rows_ignore_the_final_newline() {
        let csv = |text: &str| {
            outline::generate(
                Path::new("t.csv"),
                FileType::Tabular,
                text,
                text.as_bytes(),
                false,
            )
        };
        let out = csv("a,b\n1,2\n3,4\n");
        assert!(out.contains("rows: 2"), "{out}");
        let unterminated = csv("a,b\n1,2\n3,4");
        assert!(unterminated.contains("rows: 2"), "{unterminated}");
    }

    /// The hash-anchored view keeps a byte-order mark: `tilth_write` checks
    /// anchors against the file as it is on disk, mark included, so a view
    /// that dropped it would hand out a line-1 anchor that never matches.
    #[test]
    fn hashline_view_keeps_the_byte_order_mark() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.py");
        std::fs::write(&path, "\u{feff}import os\n").unwrap();
        let cache = OutlineCache::new();
        let out = read_file(&path, None, Some(full_cap()), &cache, true, None).unwrap();
        let hash = format::line_hash("\u{feff}import os".as_bytes());
        assert!(out.contains(&format!("1:{hash:03x}|")), "{out}");
    }

    /// A file under the outline threshold but over the budget was read in
    /// full and cut at the budget, so it showed `entry_0`'s body and hid
    /// `entry_1` to `entry_7`. Its outline fits, and lists all eight.
    #[test]
    fn a_budget_the_full_file_overflows_gets_the_outline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("medium.rs");
        let mut src = String::new();
        for i in 0..8 {
            writeln!(src, "pub fn entry_{i}() {{").unwrap();
            src.push_str(&"    let body_only_marker = 123456789;\n".repeat(30));
            src.push_str("}\n\n");
        }
        assert!(estimate_tokens(src.len() as u64) < TOKEN_THRESHOLD);
        std::fs::write(&path, &src).unwrap();
        let cache = OutlineCache::new();
        let unbudgeted = read_file(&path, None, None, &cache, false, None).unwrap();
        assert!(unbudgeted.contains("[full]"), "{unbudgeted}");

        let out = read_file(&path, None, None, &cache, false, Some(500)).unwrap();
        assert!(out.contains("[outline]"), "{out}");
        assert!(out.contains("fn entry_7"), "{out}");
        assert!(!out.contains("body_only_marker"), "{out}");
        assert!(estimate_tokens(out.len() as u64) <= 500, "{out}");
        // Asked for in full, the file is what was asked for.
        let full = read_file(&path, None, Some(full_cap()), &cache, false, Some(500)).unwrap();
        assert!(full.contains("[full]"), "{full}");
    }

    /// An outline over the budget was cut mid-entry, so `third` was gone
    /// and the note gave only a token count. Signatures and docs go first,
    /// every name stays, and the note says so.
    #[test]
    fn an_outline_over_the_budget_keeps_every_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("service.ts");
        let mut src = String::new();
        for name in ["first", "second", "third"] {
            writeln!(src, "/** {} */", "Documented at length. ".repeat(6)).unwrap();
            let params: Vec<String> = (0..12).map(|i| format!("argument_{i}: string")).collect();
            writeln!(
                src,
                "export function {name}({}): void {{",
                params.join(", ")
            )
            .unwrap();
            src.push_str(&"  const filler = 'x'.repeat(100);\n".repeat(300));
            src.push_str("}\n");
        }
        assert!(estimate_tokens(src.len() as u64) > TOKEN_THRESHOLD);
        std::fs::write(&path, &src).unwrap();
        let cache = OutlineCache::new();
        let budget = 150;
        let unbudgeted = read_file(&path, None, None, &cache, false, None).unwrap();
        assert!(
            estimate_tokens(unbudgeted.len() as u64) > budget,
            "{unbudgeted}"
        );

        let out = read_file(&path, None, None, &cache, false, Some(budget)).unwrap();
        for name in ["fn first", "fn second", "fn third"] {
            assert!(out.contains(name), "{name} missing:\n{out}");
        }
        assert!(!out.contains("argument_0"), "{out}");
        assert!(out.contains("signatures and docs left out"), "{out}");
        assert!(!out.contains("entries shown"), "{out}");
        assert!(estimate_tokens(out.len() as u64) <= budget, "{out}");
    }

    /// When even the names do not fit, whole entries go from the end, and
    /// the note counts them.
    #[test]
    fn an_outline_cut_by_entries_counts_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("many.rs");
        let mut src = String::new();
        for i in 0..60 {
            writeln!(src, "pub fn function_number_{i}() {{").unwrap();
            src.push_str(&"    let filler = 1234567890;\n".repeat(20));
            src.push_str("}\n");
        }
        assert!(estimate_tokens(src.len() as u64) > TOKEN_THRESHOLD);
        std::fs::write(&path, &src).unwrap();
        let cache = OutlineCache::new();
        let budget = 200;
        let out = read_file(&path, None, None, &cache, false, Some(budget)).unwrap();
        assert!(out.contains("fn function_number_0\n"), "{out}");
        assert!(!out.contains("function_number_59"), "{out}");
        let shown = out.lines().filter(|l| l.starts_with('[')).count();
        assert!(
            out.contains(&format!(
                "signatures and docs left out; {shown} of 60 entries shown"
            )),
            "{out}"
        );
        assert!(estimate_tokens(out.len() as u64) <= budget, "{out}");
    }
}
