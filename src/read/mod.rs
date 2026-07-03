pub mod fuzzy_path;
pub mod imports;
pub mod outline;

use std::fmt::Write;
use std::fs;
use std::path::{Component, Path, PathBuf};

use memmap2::Mmap;

use crate::cache::OutlineCache;
use crate::error::TilthError;
use crate::format;
use crate::lang::detect_file_type;
use crate::lang::outline::{heading_level, heading_text, parse_markdown};
use crate::types::{estimate_tokens, FileType, ViewMode};

pub(crate) const TOKEN_THRESHOLD: u64 = 6_000;
const FILE_SIZE_CAP: u64 = 500_000; // 500KB

/// Max file size for `full=true` reads. Files above this threshold get a
/// warning header + outline instead of raw content, preventing multi-megabyte
/// responses that cause MCP client timeouts.
/// Override with `TILTH_FULL_SIZE_CAP` env var (bytes). Default: 2MB.
fn full_read_size_cap() -> u64 {
    std::env::var("TILTH_FULL_SIZE_CAP")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(2_000_000)
}

/// True if `path` is denied by a `.tilthignore` file in its directory ancestry
/// (gitignore syntax, closest file wins, `!` re-include supported). Walks up to
/// and including the repo root (first ancestor containing `.git`), else the
/// filesystem root.
///
/// The walker honors `.tilthignore` for search/list/map automatically, but an
/// explicit `tilth_read` bypasses the walker — this is the hook that lets a repo
/// hard-deny a deliberate read of its secret files. Built-in secret files
/// (`.env`, keys) are intentionally NOT blocked here: reading one explicitly is
/// a deliberate act; only a repo's own `.tilthignore` makes a read fail.
pub(crate) fn tilthignore_denies(path: &Path) -> bool {
    use ignore::gitignore::GitignoreBuilder;

    let Some(abs) = absolute_lexical(path) else {
        return false;
    };

    let mut dir = abs.parent();
    while let Some(d) = dir {
        let ignore_file = d.join(crate::search::TILTHIGNORE_FILE);
        if ignore_file.is_file() {
            let mut builder = GitignoreBuilder::new(d);
            // `add` returns Some(Error) for partial parse errors — those are
            // advisory (the rest of the file still applies). Call `build()`
            // regardless so valid patterns are honoured even alongside bad ones.
            let _ = builder.add(&ignore_file);
            match builder.build() {
                Ok(gi) => {
                    // Closest .tilthignore wins — first definitive verdict ends it.
                    let verdict = gi.matched_path_or_any_parents(&abs, false);
                    if verdict.is_ignore() {
                        return true;
                    }
                    if verdict.is_whitelist() {
                        return false;
                    }
                }
                Err(_) => {
                    // Failed to build a matcher — fail closed to protect secrets.
                    return true;
                }
            }
        }
        if d.join(".git").exists() {
            break; // stop at the repo root
        }
        dir = d.parent();
    }
    false
}

fn absolute_lexical(path: &Path) -> Option<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(_) | Component::RootDir | Component::Prefix(_) => {
                out.push(component.as_os_str());
            }
        }
    }
    Some(out)
}

pub(crate) fn blocked_notice(path: &Path) -> String {
    format!("# {}\nblocked: denied by .tilthignore", path.display())
}

/// Main entry point for read mode. Routes through the decision tree.
pub fn read_file(
    path: &Path,
    section: Option<&str>,
    full: bool,
    cache: &OutlineCache,
    edit_mode: bool,
) -> Result<String, TilthError> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(TilthError::NotFound {
                path: path.to_path_buf(),
                suggestion: suggest_similar(path),
            });
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(TilthError::PermissionDenied {
                path: path.to_path_buf(),
            });
        }
        Err(e) => {
            return Err(TilthError::IoError {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };

    // Directory → list contents
    if meta.is_dir() {
        return list_directory(path);
    }

    let byte_len = meta.len();

    // Empty check before mmap — mmap on 0-byte file may fail on some platforms
    if byte_len == 0 {
        return Ok(format::file_header(path, 0, 0, ViewMode::Empty));
    }

    // Section param → return those lines verbatim, any size
    if let Some(range) = section {
        return read_ranges(path, &[range], edit_mode);
    }

    // Binary detection
    let file = fs::File::open(path).map_err(|e| TilthError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mmap = unsafe { Mmap::map(&file) }.map_err(|e| TilthError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;
    let buf = &mmap[..];

    if crate::lang::detection::is_binary(buf) {
        let mime = mime_from_ext(path);
        return Ok(format::binary_header(path, byte_len, mime));
    }

    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

    // Generated
    if crate::lang::detection::is_generated_by_name(name)
        || crate::lang::detection::is_generated_by_content(buf)
    {
        let line_count = memchr::memchr_iter(b'\n', buf).count() as u32 + 1;
        return Ok(format::file_header(
            path,
            byte_len,
            line_count,
            ViewMode::Generated,
        ));
    }

    // Minified — filename convention or, for big files, newline-density heuristic.
    if crate::lang::detection::is_minified_by_name(name)
        || (byte_len >= crate::lang::detection::MINIFIED_CHECK_THRESHOLD
            && crate::lang::detection::is_minified_by_content(buf))
    {
        let line_count = memchr::memchr_iter(b'\n', buf).count() as u32 + 1;
        return Ok(format::file_header(
            path,
            byte_len,
            line_count,
            ViewMode::Minified,
        ));
    }

    let tokens = estimate_tokens(byte_len);
    let content = String::from_utf8_lossy(buf);
    let line_count = memchr::memchr_iter(b'\n', buf).count() as u32 + 1;

    // Guard: full=true on very large files. Return outline + warning instead of
    // dumping megabytes that would blow up the MCP client's timeout/memory.
    let cap = full_read_size_cap();
    if full && byte_len > cap {
        let file_type = detect_file_type(path);
        let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        #[allow(clippy::cast_precision_loss)] // cap and file sizes fit in f64 mantissa for display
        let cap_mb = cap as f64 / 1_000_000.0;
        #[allow(clippy::cast_precision_loss)]
        let file_mb = byte_len as f64 / 1_000_000.0;

        let outline = cache.get_or_compute(path, mtime, || {
            outline::generate(path, file_type, &content, buf, true)
        });

        let header = format::file_header(path, byte_len, line_count, ViewMode::Outline);
        return Ok(format!(
            "{header}\n\n> **full=true skipped**: file is {file_mb:.1}MB (cap: {cap_mb:.1}MB). \
             Use `section` to read specific ranges, or set TILTH_FULL_SIZE_CAP={byte_len} to override.\n\n{outline}"
        ));
    }

    // Full mode or small file → return full content (skip smart view)
    if full || tokens <= TOKEN_THRESHOLD {
        let header = format::file_header(path, byte_len, line_count, ViewMode::Full);
        if edit_mode {
            return Ok(edit_whole_view(path, &content, &header));
        }
        return Ok(format!("{header}\n\n{content}"));
    }

    // Large file → smart view by file type
    let file_type = detect_file_type(path);
    let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);

    let capped = byte_len > FILE_SIZE_CAP;

    let outline = cache.get_or_compute(path, mtime, || {
        outline::generate(path, file_type, &content, buf, capped)
    });

    let mode = match file_type {
        FileType::StructuredData => ViewMode::Keys,
        _ => ViewMode::Outline,
    };
    let header = format::file_header(path, byte_len, line_count, mode);
    Ok(format!("{header}\n\n{outline}"))
}

/// Read `path`, and on a missing path attempt fuzzy resolution against `scope`.
///
/// Cold-path wrapper around [`read_file`]: on `NotFound`, scores the (scope-
/// relative) query against the gitignore-pruned tree. A confident `Resolved`
/// auto-opens the winning file with a `# <real-path> (corrected from "<query>")`
/// header; `Suggestions` enrich the `NotFound` with a ranked "did you mean"
/// list; `None` returns the unchanged `NotFound` (today's behaviour). A
/// successful read never walks — it returns `read_file`'s result untouched.
/// Reduce `path` to a scope-relative query string for fuzzy matching.
///
/// Candidates from the walker are relative to `scope`, so the query must be too.
/// Stripping the scope prefix handles the common case, but a *relative* `scope`
/// (e.g. the MCP layer's `"."`) strips nothing from an absolute caller path, so
/// the query would stay absolute and never subsequence-match a relative
/// candidate. When the strip leaves the query absolute, re-strip against the
/// canonical scope so an absolute MCP path still reduces to the relative form.
fn scope_relative_query<'a>(path: &'a Path, scope: &Path) -> std::borrow::Cow<'a, str> {
    let stripped = path.strip_prefix(scope).unwrap_or(path);
    if stripped.is_absolute() {
        if let Ok(abs_scope) = scope.canonicalize() {
            return stripped
                .strip_prefix(&abs_scope)
                .unwrap_or(stripped)
                .to_string_lossy();
        }
    }
    stripped.to_string_lossy()
}

pub fn read_file_resolving(
    path: &Path,
    section: Option<&str>,
    full: bool,
    cache: &OutlineCache,
    edit_mode: bool,
    scope: &Path,
) -> Result<String, TilthError> {
    // Only NotFound triggers a walk; every other outcome (success or other
    // error) returns untouched, so a successful read never pays for the tree walk.
    let (missing, suggestion) = match read_file(path, section, full, cache, edit_mode) {
        Err(TilthError::NotFound { path, suggestion }) => (path, suggestion),
        other => return other,
    };

    // The query is the scope-relative path the caller asked for.
    let query = scope_relative_query(path, scope);
    match fuzzy_path::resolve_fuzzy_path(scope, &query, fuzzy_path::GateProfile::Read) {
        fuzzy_path::FuzzyResolution::Resolved(hit) => {
            hit.log_auto_open(&query);
            let real = scope.join(&hit.path);
            let body = read_file(&real, section, full, cache, edit_mode)?;
            Ok(format!(
                "# {} (corrected from \"{query}\")\n\n{body}",
                hit.path.display()
            ))
        }
        fuzzy_path::FuzzyResolution::Suggestions(s) => Err(TilthError::NotFound {
            path: missing,
            suggestion: Some(s.join(", ")),
        }),
        fuzzy_path::FuzzyResolution::None => Err(TilthError::NotFound {
            path: missing,
            suggestion,
        }),
    }
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

    #[allow(clippy::cast_possible_truncation)]
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
        parse_range(range).ok_or_else(|| TilthError::InvalidQuery {
            query: range.to_string(),
            reason: "expected format: \"start-end\" (e.g. \"45-89\") or heading (e.g. \"## Architecture\")".into(),
        })
    }
}

/// One resolved range, ready to format.
struct Block {
    start: usize, // 1-indexed inclusive
    end: usize,   // 1-indexed inclusive (clamped to file length)
    text: String,
}

/// Edit-mode whole-file view: tilth's `# path (...) [full]` header, then the
/// whole-file-tag section header `[path#TAG]` and `N:content` numbered lines
/// (`split('\n')`, phantom trailing row included). Files over the per-file
/// snapshot cap mint no tag (spec) — they render numbered lines with a plain
/// `# <path>` marker so the model can read but not tag-verify an edit.
fn edit_whole_view(path: &Path, content: &str, file_header: &str) -> String {
    let numbered = crate::edit::tag::render_numbered_whole(content);
    if content.len() > crate::edit::snapshots::DEFAULT_PER_FILE_CAP {
        return format!(
            "{file_header}\n\n# {} (too large to tag; edits cannot be tag-verified)\n{numbered}",
            path.display()
        );
    }
    let tag = crate::edit::tag::compute_file_hash(content);
    let tag_header = crate::edit::tag::format_header(&path.display().to_string(), tag);
    format!("{file_header}\n\n{tag_header}\n{numbered}")
}

/// Which lines an edit-mode read displayed, for the seen-lines provenance gate.
pub enum SeenSpec {
    /// The whole file was displayed (all `split('\n')` rows are seen).
    Whole,
    /// Only these 1-based inclusive ranges were displayed.
    Ranges(Vec<(u32, u32)>),
}

/// Record the whole-file-tag snapshot for an edit-mode read into the session
/// store, tagged by the file's live content and stamped with the line numbers
/// the read displayed. Best-effort: an unreadable file (binary, deleted mid-
/// call) records nothing. Keyed by canonical realpath so a later `tilth_write`
/// finds the snapshot regardless of path spelling.
pub fn record_edit_snapshot(session: &crate::session::Session, path: &Path, spec: &SeenSpec) {
    // `SnapshotStore::record` drops anything over the per-file cap — skip
    // the full-file read and seen materialization entirely for those.
    let over_cap = fs::metadata(path)
        .is_ok_and(|m| m.len() > crate::edit::snapshots::DEFAULT_PER_FILE_CAP as u64);
    if over_cap {
        return;
    }
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let seen: Vec<u32> = match spec {
        SeenSpec::Whole => {
            let n = u32::try_from(text.split('\n').count()).unwrap_or(u32::MAX);
            (1..=n).collect()
        }
        SeenSpec::Ranges(ranges) => ranges.iter().flat_map(|&(s, e)| s..=e.max(s)).collect(),
    };
    let key = crate::edit::normalize_path_key(path);
    session.record_snapshot(&key, &text, seen);
}

/// Resolve a markdown `#heading` anchor to its 1-based inclusive line span,
/// for the seen-lines provenance of an edit-mode heading read. Returns `None`
/// when the file is unreadable or the heading is absent (caller falls back).
pub fn resolve_heading_span(path: &Path, heading: &str) -> Option<(u32, u32)> {
    let bytes = fs::read(path).ok()?;
    let (start, end) = resolve_heading(&bytes, heading)?;
    Some((
        u32::try_from(start).unwrap_or(u32::MAX),
        u32::try_from(end).unwrap_or(u32::MAX),
    ))
}

/// Read one or more line ranges from a file. Each range is "start-end"
/// (e.g. "45-89") or a heading anchor (e.g. "## Architecture") for
/// markdown files. Mmaps the file once and emits a single `[section]`
/// header followed by the formatted blocks; when more than one range is
/// requested, each block is preceded by a `─── lines X-Y ───` delimiter.
///
/// Ranges are emitted in the order supplied — overlapping or out-of-order
/// ranges are honored verbatim, not coalesced or sorted. Any invalid
/// range fails the whole call.
pub fn read_ranges(path: &Path, ranges: &[&str], edit_mode: bool) -> Result<String, TilthError> {
    if ranges.is_empty() {
        return Err(TilthError::InvalidQuery {
            query: String::new(),
            reason: "at least one range is required".into(),
        });
    }

    let file = fs::File::open(path).map_err(|e| TilthError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mmap = unsafe { Mmap::map(&file) }.map_err(|e| TilthError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;
    let buf = &mmap[..];

    // Find line offsets once — shared across all ranges.
    let mut line_offsets: Vec<usize> = vec![0];
    for pos in memchr::memchr_iter(b'\n', buf) {
        line_offsets.push(pos + 1);
    }
    let total = line_offsets.len();

    // Edit-mode sections carry the WHOLE-file content tag (not a per-slice
    // tag) so a follow-up tilth_write verifies against live file content.
    let tag_header = if edit_mode {
        let whole = String::from_utf8_lossy(buf);
        let tag = crate::edit::tag::compute_file_hash(&whole);
        Some(crate::edit::tag::format_header(
            &path.display().to_string(),
            tag,
        ))
    } else {
        None
    };

    let mut blocks: Vec<Block> = Vec::with_capacity(ranges.len());
    let mut total_bytes: u64 = 0;
    let mut total_lines: u32 = 0;

    for range in ranges {
        let (start, end) = resolve_range(buf, range)?;
        let s = start.saturating_sub(1).min(total);
        let e = end.min(total);
        if s >= e {
            return Err(TilthError::InvalidQuery {
                query: (*range).to_string(),
                reason: format!("range out of bounds (file has {total} lines)"),
            });
        }
        let start_byte = line_offsets[s];
        let end_byte = if e < line_offsets.len() {
            line_offsets[e]
        } else {
            buf.len()
        };
        let selected = String::from_utf8_lossy(&buf[start_byte..end_byte]);
        total_bytes += selected.len() as u64;
        total_lines += (e - s) as u32;
        let formatted = if edit_mode {
            crate::edit::tag::render_numbered_slice(&selected, start as u32)
        } else {
            format::number_lines(&selected, start as u32)
        };
        blocks.push(Block {
            start,
            end: e,
            text: formatted,
        });
    }

    let header = format::file_header(path, total_bytes, total_lines, ViewMode::Section);
    // In edit mode the tag header sits between the file header and the blocks.
    let header = match &tag_header {
        Some(t) => format!("{header}\n\n{t}"),
        None => header,
    };

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
            _ => match meta {
                Some(m) => {
                    let tokens = estimate_tokens(m.len());
                    format!("  ({tokens} tokens)")
                }
                None => String::new(),
            },
        };
        entries.push(format!("  {name}{suffix}"));
    }

    let header = format!("# {} ({} items)", path.display(), items.len());
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
fn mime_from_ext(path: &Path) -> &'static str {
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

    #[test]
    fn full_true_size_cap_returns_outline() {
        use std::io::Write;

        // Create a temp file larger than our small cap (100 bytes)
        let path = std::env::temp_dir().join("tilth_test_large.rs");
        let mut f = std::fs::File::create(&path).unwrap();
        // Write enough to exceed the cap — 200 bytes of Rust code
        for i in 0..20 {
            writeln!(f, "pub fn func_{i}() {{ println!(\"hello\"); }}").unwrap();
        }
        drop(f);

        // Set a tiny cap so the guard triggers
        std::env::set_var("TILTH_FULL_SIZE_CAP", "100");

        let cache = OutlineCache::new();
        let result = read_file(&path, None, true, &cache, false).unwrap();

        // Should contain the warning, not the full file content
        assert!(
            result.contains("full=true skipped"),
            "expected size cap warning, got: {result}"
        );
        assert!(
            result.contains("func_0"),
            "expected outline content in output"
        );

        std::env::remove_var("TILTH_FULL_SIZE_CAP");
        let _ = std::fs::remove_file(&path);
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
        assert!(suggest_headings(input, "", 5).is_empty());
        assert!(suggest_headings(input, "###", 5).is_empty());
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
    fn read_file_resolving_auto_opens_with_correction_header() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/search")).unwrap();
        std::fs::write(
            dir.path().join("src/search/symbol.rs"),
            "pub fn find() {}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub mod search;\n").unwrap();

        let cache = OutlineCache::new();
        // Basename-only miss resolves to the unique real file.
        let missing = dir.path().join("symbol.rs");
        let out = read_file_resolving(&missing, None, false, &cache, false, dir.path()).unwrap();

        assert!(
            out.contains("src/search/symbol.rs (corrected from \"symbol.rs\")"),
            "expected correction header, got: {out}"
        );
        assert!(
            out.contains("pub fn find"),
            "expected resolved file body: {out}"
        );
    }

    #[test]
    fn scope_relative_query_strips_absolute_under_dot_scope() {
        // The MCP read entry passes scope="." with a caller-supplied path that may
        // be absolute. `strip_prefix(".")` strips nothing, so without the canonical
        // fallback the query would stay absolute and never match a scope-relative
        // candidate. Reads (not mutates) cwd, so it's safe under parallel tests.
        let cwd = std::env::current_dir().unwrap();
        let absolute = cwd.join("src/serch/symbol.rs");
        let query = scope_relative_query(&absolute, Path::new("."));
        assert_eq!(
            query, "src/serch/symbol.rs",
            "absolute path under a '.' scope must reduce to the scope-relative query"
        );
    }

    #[test]
    fn scope_relative_query_absolute_scope_and_relative_input_unchanged() {
        // Absolute scope + absolute path under it strips directly (the existing
        // tempdir tests rely on this; no canonicalization, so macOS /var symlinks
        // don't bite). A relative input is returned untouched.
        let scope = Path::new("/abs/proj");
        assert_eq!(
            scope_relative_query(Path::new("/abs/proj/src/foo.rs"), scope),
            "src/foo.rs"
        );
        assert_eq!(
            scope_relative_query(Path::new("src/bar.rs"), scope),
            "src/bar.rs"
        );
    }

    #[test]
    fn read_file_resolving_exact_path_is_byte_identical() {
        // An existing path must not walk and must match plain read_file exactly.
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("present.rs");
        std::fs::write(&real, "fn main() {}\n").unwrap();

        let cache = OutlineCache::new();
        let plain = read_file(&real, None, false, &cache, false).unwrap();
        let resolving = read_file_resolving(&real, None, false, &cache, false, dir.path()).unwrap();
        assert_eq!(plain, resolving, "happy-path read must be byte-identical");
    }

    #[test]
    fn read_file_resolving_ambiguous_enriches_notfound_suggestion() {
        // An ambiguous basename miss must NOT auto-open; it returns NotFound
        // enriched with the ranked candidates as a "did you mean" list.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a")).unwrap();
        std::fs::create_dir_all(dir.path().join("b")).unwrap();
        std::fs::write(dir.path().join("a/mod.rs"), "// a\n").unwrap();
        std::fs::write(dir.path().join("b/mod.rs"), "// b\n").unwrap();

        let cache = OutlineCache::new();
        let missing = dir.path().join("mod.rs");
        let err = read_file_resolving(&missing, None, false, &cache, false, dir.path())
            .expect_err("ambiguous basename must stay NotFound, not auto-open");
        let TilthError::NotFound { suggestion, .. } = err else {
            panic!("expected NotFound, got: {err:?}");
        };
        let suggestion = suggestion.expect("ambiguous miss must carry suggestions");
        assert!(
            suggestion.contains("a/mod.rs") && suggestion.contains("b/mod.rs"),
            "both candidates must appear in the suggestion: {suggestion}"
        );
    }

    #[test]
    fn read_file_resolving_garbage_stays_not_found() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real.rs"), "fn main() {}\n").unwrap();

        let cache = OutlineCache::new();
        let missing = dir.path().join("zzqq/nonexistent_xyzzy.bin");
        let err = read_file_resolving(&missing, None, false, &cache, false, dir.path())
            .expect_err("garbage path must stay NotFound");
        assert!(
            matches!(err, TilthError::NotFound { .. }),
            "expected NotFound, got: {err:?}"
        );
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
    fn read_ranges_edit_mode_emits_tag_header_and_numbered_lines_per_block() {
        // Edit-mode section output carries the WHOLE-file tag header once, then
        // per-block `N:content` numbered lines (no per-line hash — that is the
        // whole-file-tag token win).
        let content = "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\n";
        let path = write_temp("tilth_test_ranges_edit_mode.txt", content);
        let out = read_ranges(&path, &["1-2", "5-6"], true).unwrap();
        // Both delimiters present.
        assert!(out.contains("─── lines 1-2 ───"), "first delimiter: {out}");
        assert!(out.contains("─── lines 5-6 ───"), "second delimiter: {out}");
        // The whole-file tag header appears exactly once.
        let expected_tag =
            crate::edit::tag::format_tag(crate::edit::tag::compute_file_hash(content));
        assert!(
            out.contains(&format!("#{expected_tag}]")),
            "expected whole-file [path#{expected_tag}] header: {out}"
        );
        // Numbered lines are `N:content`, no `|hash` prefix.
        assert!(
            out.lines().any(|l| l == "1:alpha"),
            "expected `1:alpha` numbered line: {out}"
        );
        assert!(
            out.lines().any(|l| l == "6:zeta"),
            "expected `6:zeta` numbered line: {out}"
        );
        assert!(
            !out.contains("|alpha"),
            "per-line hash prefix must be gone in edit mode: {out}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tilthignore_denies_listed_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".tilthignore"), "cluster/.env\n").unwrap();
        std::fs::create_dir_all(dir.path().join("cluster")).unwrap();
        let secret = dir.path().join("cluster/.env");
        std::fs::write(&secret, "API_KEY=abc\n").unwrap();

        assert!(
            tilthignore_denies(&secret),
            "path listed in .tilthignore must be denied"
        );
    }

    #[test]
    fn tilthignore_allows_unlisted_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".tilthignore"), "secrets/\n").unwrap();
        let ok = dir.path().join("main.rs");
        std::fs::write(&ok, "fn main() {}\n").unwrap();

        assert!(!tilthignore_denies(&ok), "unlisted path must not be denied");
    }

    #[test]
    fn tilthignore_does_not_block_plain_secret_read() {
        // A built-in secret file with no .tilthignore is still explicitly
        // readable — only a repo's .tilthignore makes a deliberate read fail.
        let dir = tempfile::tempdir().unwrap();
        let env = dir.path().join(".env");
        std::fs::write(&env, "API_KEY=abc\n").unwrap();

        assert!(
            !tilthignore_denies(&env),
            "secrets denylist must not block explicit reads; only .tilthignore does"
        );
    }

    #[test]
    fn tilthignore_nested_closest_wins() {
        // A parent .tilthignore denies all *.env; a child .tilthignore in a
        // subdirectory re-includes one of them via `!`. The closest file wins.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".tilthignore"), "*.env\n").unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(".tilthignore"), "!keep.env\n").unwrap();

        let kept = sub.join("keep.env");
        let denied_child = sub.join("other.env");
        let denied_root = dir.path().join("root.env");
        std::fs::write(&kept, "x").unwrap();
        std::fs::write(&denied_child, "y").unwrap();
        std::fs::write(&denied_root, "z").unwrap();

        assert!(
            !tilthignore_denies(&kept),
            "child `!keep.env` must override the parent `*.env` deny"
        );
        assert!(
            tilthignore_denies(&denied_child),
            "other.env in the child dir is still caught by the parent `*.env`"
        );
        assert!(
            tilthignore_denies(&denied_root),
            "root.env is denied by the root `*.env`"
        );
    }

    #[cfg(unix)]
    #[test]
    fn tilthignore_denies_symlink_path_without_resolving_target() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".tilthignore"), "secret.env\n").unwrap();
        let target = outside.path().join("target.env");
        std::fs::write(&target, "API_KEY=abc\n").unwrap();
        let link = dir.path().join("secret.env");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(
            tilthignore_denies(&link),
            ".tilthignore must match the requested symlink path, not the resolved target"
        );
    }

    #[test]
    fn tilthignore_negation_reincludes_for_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".tilthignore"), "*.env\n!keep.env\n").unwrap();
        let denied = dir.path().join("prod.env");
        let kept = dir.path().join("keep.env");
        std::fs::write(&denied, "x").unwrap();
        std::fs::write(&kept, "y").unwrap();

        assert!(tilthignore_denies(&denied), "prod.env should be denied");
        assert!(
            !tilthignore_denies(&kept),
            "`!keep.env` should re-allow keep.env"
        );
    }

    /// Regression: a malformed glob pattern in .tilthignore (e.g. unclosed `[`)
    /// must NOT cause the entire file to be skipped, leaving secrets readable.
    /// The `ignore` crate treats parse errors as advisory — valid patterns still
    /// apply. This test confirms a secret listed after a bad pattern is denied.
    #[test]
    fn tilthignore_malformed_pattern_still_denies_secret() {
        let dir = tempfile::tempdir().unwrap();
        // First line: bad glob (unclosed character class). Second line: real deny.
        std::fs::write(dir.path().join(".tilthignore"), "[invalid\nsecret.env\n").unwrap();
        let secret = dir.path().join("secret.env");
        std::fs::write(&secret, "API_KEY=abc\n").unwrap();

        assert!(
            tilthignore_denies(&secret),
            "secret.env must be denied even when .tilthignore contains a malformed pattern"
        );
    }
}
