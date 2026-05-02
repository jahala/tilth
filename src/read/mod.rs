pub mod imports;
pub mod outline;

use std::fs;
use std::path::Path;

use memmap2::Mmap;

use crate::cache::OutlineCache;
use crate::error::TilthError;
use crate::format;
use crate::lang::detect_file_type;
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
        return read_section(path, range, edit_mode);
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
            let numbered = format::hashlines(&content, 1);
            return Ok(format!("{header}\n\n{numbered}"));
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

/// Would this file produce an outline (rather than full content) in default read mode?
/// Used by the MCP layer to decide whether to append related-file hints.
pub fn would_outline(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| !m.is_dir() && estimate_tokens(m.len()) > TOKEN_THRESHOLD)
}

/// Resolve a heading address to a line range in a markdown file.
/// Returns `(start_line, end_line)` as 1-indexed inclusive range.
/// Returns `None` if heading not found.
fn resolve_heading(buf: &[u8], heading: &str) -> Option<(usize, usize)> {
    let heading_trimmed = heading.trim_end();
    let heading_level = heading_trimmed.chars().take_while(|&c| c == '#').count();

    if heading_level == 0 {
        return None;
    }

    // Build line offsets
    let mut line_offsets: Vec<usize> = vec![0];
    for pos in memchr::memchr_iter(b'\n', buf) {
        line_offsets.push(pos + 1);
    }
    // Exclude phantom empty line after trailing newline (match outline's count)
    let total_lines = if buf.last() == Some(&b'\n') {
        line_offsets.len() - 1
    } else {
        line_offsets.len()
    };

    let mut in_code_block = false;
    let mut found_line: Option<usize> = None;

    // Scan for the target heading
    for (line_idx, &offset) in line_offsets.iter().enumerate() {
        let line_end = if line_idx + 1 < line_offsets.len() {
            line_offsets[line_idx + 1] - 1 // exclude newline
        } else {
            buf.len()
        };

        if let Ok(line_str) = std::str::from_utf8(&buf[offset..line_end]) {
            let trimmed = line_str.trim_end();

            // Track code blocks
            if trimmed.starts_with("```") {
                in_code_block = !in_code_block;
                continue;
            }

            // Skip headings inside code blocks
            if in_code_block {
                continue;
            }

            // Check if this line matches the heading
            if trimmed == heading_trimmed {
                found_line = Some(line_idx + 1); // 1-indexed
                break;
            }
        }
    }

    let start_line = found_line?;

    // Find the next heading of same or higher level
    in_code_block = false;
    let start_idx = start_line - 1; // convert back to 0-indexed for iteration

    for (line_idx, &offset) in line_offsets.iter().enumerate().skip(start_idx + 1) {
        let line_end = if line_idx + 1 < line_offsets.len() {
            line_offsets[line_idx + 1] - 1
        } else {
            buf.len()
        };

        if let Ok(line_str) = std::str::from_utf8(&buf[offset..line_end]) {
            let trimmed = line_str.trim_end();

            if trimmed.starts_with("```") {
                in_code_block = !in_code_block;
                continue;
            }

            if in_code_block {
                continue;
            }

            // Check if this is a heading
            if trimmed.starts_with('#') {
                let level = trimmed.chars().take_while(|&c| c == '#').count();
                if level <= heading_level {
                    // 0-based line_idx of next heading = 1-indexed line before it
                    return Some((start_line, line_idx));
                }
            }
        }
    }

    // No next heading found — section goes to end of file
    Some((start_line, total_lines))
}

/// Return up to `top_n` markdown headings ranked by edit distance to `query`.
///
/// Used when a heading lookup misses — agents typo'd anchors, or the heading
/// renamed since they last read. Returning the closest matches lets them
/// retry with the right anchor without re-reading the whole file.
///
/// Recognizes `ATX` headings (`CommonMark` §4.6: 1–6 leading `#`s followed
/// by a space). Skips headings inside fenced code blocks delimited by either
/// triple backticks or triple tildes. `Setext` headings (`Title\n===`) are
/// not recognized — they are rare in practice and would require lookback.
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

    let mut in_code_block = false;
    let mut scored: Vec<(usize, String)> = Vec::new();
    for line in buf.split(|&b| b == b'\n') {
        let Ok(s) = std::str::from_utf8(line) else {
            continue;
        };
        let trimmed = s.trim_end();
        // CommonMark allows both ``` and ~~~ as fence delimiters.
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block || !trimmed.starts_with('#') {
            continue;
        }
        // CommonMark §4.6.1: ATX headings have 1–6 `#`s followed by a space
        // (or end-of-line). Reject longer hash runs and hash-prefixes that
        // aren't followed by whitespace (`##foo` is not a heading).
        let hash_count = trimmed.bytes().take_while(|&b| b == b'#').count();
        if !(1..=6).contains(&hash_count) {
            continue;
        }
        let after_hashes = trimmed.as_bytes().get(hash_count).copied();
        if !matches!(after_hashes, None | Some(b' ' | b'\t')) {
            continue;
        }
        let h_text = trimmed[hash_count..].trim();
        if h_text.is_empty() {
            continue;
        }
        // Strip kramdown attr blocks and ATX-close `#`s from comparison text.
        let h_clean = h_text
            .split('{')
            .next()
            .unwrap_or(h_text)
            .trim_end_matches('#')
            .trim();
        let dist = edit_distance(&q_lower, &h_clean.to_ascii_lowercase());
        scored.push((dist, trimmed.to_string()));
    }

    scored.sort_by_key(|(d, _)| *d);
    scored.into_iter().take(top_n).map(|(_, h)| h).collect()
}

/// Read a specific line range from a file.
/// Uses memchr to find the Nth newline offset and slice the mmap buffer directly
/// instead of collecting all lines into a Vec.
fn read_section(path: &Path, range: &str, edit_mode: bool) -> Result<String, TilthError> {
    let file = fs::File::open(path).map_err(|e| TilthError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mmap = unsafe { Mmap::map(&file) }.map_err(|e| TilthError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;
    let buf = &mmap[..];

    // Check if this is a heading-based address (markdown)
    let (start, end) = if range.starts_with('#') {
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
        })?
    } else {
        parse_range(range).ok_or_else(|| TilthError::InvalidQuery {
            query: range.to_string(),
            reason: "expected format: \"start-end\" (e.g. \"45-89\") or heading (e.g. \"## Architecture\")".into(),
        })?
    };

    // Find line offsets using memchr — no full-file Vec<&str> allocation
    let mut line_offsets: Vec<usize> = vec![0];
    for pos in memchr::memchr_iter(b'\n', buf) {
        line_offsets.push(pos + 1);
    }
    let total = line_offsets.len();

    let s = (start.saturating_sub(1)).min(total);
    let e = end.min(total);

    if s >= e {
        return Err(TilthError::InvalidQuery {
            query: range.to_string(),
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
    let byte_len = selected.len() as u64;
    let line_count = (e - s) as u32;
    let header = format::file_header(path, byte_len, line_count, ViewMode::Section);
    let formatted = if edit_mode {
        format::hashlines(&selected, start as u32)
    } else {
        format::number_lines(&selected, start as u32)
    };
    Ok(format!("{header}\n\n{formatted}"))
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
/// Operates on `char`s, not bytes — a single CJK or emoji glyph is one
/// edit unit, not 3-4. Byte-level Levenshtein on UTF-8 inflates distances
/// for non-ASCII content and breaks ranking for international markdown
/// or filenames. Used by both filename suggestion (`suggest_similar`) and
/// heading suggestion (`suggest_headings`).
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0; b.len() + 1];

    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
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

    /// CommonMark allows `~~~` as a fence delimiter. Headings inside
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

    /// CommonMark §4.6.1 limits ATX headings to 1–6 `#`s. Lines with 7+
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

    /// CommonMark §4.6.1: hashes must be followed by a space (or EOL).
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

    /// edit_distance must operate on `char`s so non-ASCII headings rank
    /// correctly. Byte-level Levenshtein would inflate distances for
    /// CJK or emoji because those occupy 3–4 bytes per scalar value.
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
}
