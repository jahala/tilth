//! Shared file-prefilter helper for relational queries (callers, callees,
//! deps). Reads a file, gates on size, and runs the per-file bloom prefilter
//! against any of the supplied target symbols. Returns content + mtime when
//! the file is worth deeper inspection (tree-sitter parse, outline scan).

use std::path::Path;
use std::time::SystemTime;

use crate::index::bloom::BloomFilterCache;

/// Skip files larger than this; tree-sitter parses on huge files dominate
/// query latency without surfacing useful matches.
pub(super) const MAX_FILE_SIZE: u64 = 500_000;

/// Read `path`, validate size, and pass through only when at least one
/// target is bloom-positive. Returns `(content, mtime)` for the next stage,
/// or `None` to skip the file.
///
/// Bloom is probabilistic: a positive may be a false positive. Callers that
/// need a tighter pre-AST filter (e.g. memchr) should run it on the returned
/// content before paying for tree-sitter.
pub(super) fn read_with_bloom_check<I, S>(
    path: &Path,
    targets: I,
    bloom: &BloomFilterCache,
    max_size: u64,
) -> Option<(String, SystemTime)>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > max_size {
        return None;
    }
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let content = std::fs::read_to_string(path).ok()?;

    if !targets
        .into_iter()
        .any(|t| bloom.contains(path, mtime, &content, t.as_ref()))
    {
        return None;
    }

    Some((content, mtime))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;

    #[test]
    fn returns_none_for_oversized_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("big.rs");
        // Fill past max_size
        let payload = "fn foo() {}\n".repeat(2);
        fs::write(&p, &payload).unwrap();
        let bloom = BloomFilterCache::new();
        let targets: HashSet<String> = ["foo".to_string()].into_iter().collect();
        // max_size below file len → skip
        assert!(read_with_bloom_check(&p, &targets, &bloom, 1).is_none());
    }

    #[test]
    fn returns_none_when_no_target_is_bloom_positive() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.rs");
        fs::write(&p, "fn alpha() {}\n").unwrap();
        let bloom = BloomFilterCache::new();
        let targets: HashSet<String> = ["beta".to_string()].into_iter().collect();
        assert!(read_with_bloom_check(&p, &targets, &bloom, MAX_FILE_SIZE).is_none());
    }

    #[test]
    fn returns_content_when_target_present() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.rs");
        fs::write(&p, "fn alpha() {}\n").unwrap();
        let bloom = BloomFilterCache::new();
        let targets: HashSet<String> = ["alpha".to_string()].into_iter().collect();
        let (content, _) = read_with_bloom_check(&p, &targets, &bloom, MAX_FILE_SIZE).unwrap();
        assert!(content.contains("alpha"));
    }

    #[test]
    fn accepts_borrowed_str_targets() {
        // callees.rs holds HashSet<&str>; the helper must accept that shape
        // without forcing a String allocation per call.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.rs");
        fs::write(&p, "fn alpha() {}\n").unwrap();
        let bloom = BloomFilterCache::new();
        let targets: HashSet<&str> = ["alpha"].into_iter().collect();
        assert!(read_with_bloom_check(&p, &targets, &bloom, MAX_FILE_SIZE).is_some());
    }
}
