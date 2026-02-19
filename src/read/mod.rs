pub mod binary;
pub mod generated;
pub mod imports;
pub mod outline;

use std::fs;
use std::path::Path;

use memmap2::Mmap;

use crate::cache::OutlineCache;
use crate::error::TilthError;
use crate::format;
use crate::types::{estimate_tokens, FileType, Lang, ViewMode};

pub(crate) const TOKEN_THRESHOLD: u64 = 3_500;
const FILE_SIZE_CAP: u64 = 500_000; // 500KB

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

    // Section param → return those lines verbatim, any size
    if let Some(range) = section {
        return read_section(path, range, edit_mode);
    }

    // Empty check before mmap — mmap on 0-byte file may fail on some platforms
    if byte_len == 0 {
        return Ok(format::file_header(path, 0, 0, ViewMode::Empty));
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

    if binary::is_binary(buf) {
        let mime = mime_from_ext(path);
        return Ok(format::binary_header(path, byte_len, mime));
    }

    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

    // Generated
    if generated::is_generated_by_name(name) || generated::is_generated_by_content(buf) {
        let line_count = memchr::memchr_iter(b'\n', buf).count() as u32 + 1;
        return Ok(format::file_header(
            path,
            byte_len,
            line_count,
            ViewMode::Generated,
        ));
    }

    let tokens = estimate_tokens(byte_len);
    let content = String::from_utf8_lossy(buf);
    let line_count = memchr::memchr_iter(b'\n', buf).count() as u32 + 1;

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
    std::fs::metadata(path)
        .map(|m| !m.is_dir() && estimate_tokens(m.len()) > TOKEN_THRESHOLD)
        .unwrap_or(false)
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
        resolve_heading(buf, range).ok_or_else(|| TilthError::InvalidQuery {
            query: range.to_string(),
            reason: "heading not found in file".into(),
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

/// Detect file type by extension, then by name.
pub fn detect_file_type(path: &Path) -> FileType {
    match path.extension().and_then(|e| e.to_str()) {
        Some("ts") => FileType::Code(Lang::TypeScript),
        Some("tsx") => FileType::Code(Lang::Tsx),
        Some("js" | "jsx") => FileType::Code(Lang::JavaScript),
        Some("py" | "pyi") => FileType::Code(Lang::Python),
        Some("rs") => FileType::Code(Lang::Rust),
        Some("go") => FileType::Code(Lang::Go),
        Some("java") => FileType::Code(Lang::Java),
        Some("scala" | "sc") => FileType::Code(Lang::Scala),
        Some("c" | "h") => FileType::Code(Lang::C),
        Some("cpp" | "hpp" | "cc" | "cxx") => FileType::Code(Lang::Cpp),
        Some("rb") => FileType::Code(Lang::Ruby),
        Some("swift") => FileType::Code(Lang::Swift),
        Some("kt" | "kts") => FileType::Code(Lang::Kotlin),
        Some("cs") => FileType::Code(Lang::CSharp),

        Some("md" | "mdx" | "rst") => FileType::Markdown,
        Some("json" | "yaml" | "yml" | "toml" | "xml" | "ini") => FileType::StructuredData,
        Some("csv" | "tsv") => FileType::Tabular,
        Some("log") => FileType::Log,

        None => file_type_from_name(path),
        _ => FileType::Other,
    }
}

fn file_type_from_name(path: &Path) -> FileType {
    match path.file_name().and_then(|n| n.to_str()) {
        Some("Dockerfile" | "Containerfile") => FileType::Code(Lang::Dockerfile),
        Some("Makefile" | "GNUmakefile") => FileType::Code(Lang::Make),
        Some("Vagrantfile" | "Rakefile") => FileType::Code(Lang::Ruby),
        Some(n) if n.starts_with(".env") => FileType::StructuredData,
        _ => FileType::Other,
    }
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

/// Simple Levenshtein distance — only used on short file names.
fn edit_distance(a: &str, b: &str) -> usize {
    let a = a.as_bytes();
    let b = b.as_bytes();
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
}
