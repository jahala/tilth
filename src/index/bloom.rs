//! Per-file Bloom filters for fast "does file X contain symbol Y?" queries.
//!
//! Used to pre-filter candidate files before expensive tree-sitter parsing
//! in callee/caller resolution. A Bloom filter can definitively say "no"
//! (symbol is NOT in this file) but may produce false positives.
//!
//! Identifier extraction uses a simple byte-level state machine -- no
//! tree-sitter needed -- making it fast enough to run on every uncached file.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use dashmap::DashMap;
use fastbloom::BloomFilter;

// ---------------------------------------------------------------------------
// BloomFilterCache
// ---------------------------------------------------------------------------

/// Thread-safe cache of per-file Bloom filters, keyed by path and validated
/// by mtime. Stale entries are automatically rebuilt on access.
pub struct BloomFilterCache {
    filters: DashMap<PathBuf, (BloomFilter, SystemTime)>,
}

impl Default for BloomFilterCache {
    fn default() -> Self {
        Self::new()
    }
}

impl BloomFilterCache {
    /// Create an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            filters: DashMap::new(),
        }
    }

    /// Check if `symbol` might appear in the file at `path`.
    ///
    /// - If a cached filter exists with matching `mtime`, queries it directly.
    /// - Otherwise, builds a new filter from `content`, caches it, then queries.
    ///
    /// Returns `true` if the symbol MIGHT be in the file (possible false positive),
    /// `false` if it is DEFINITELY absent.
    #[must_use]
    pub fn contains(&self, path: &Path, mtime: SystemTime, content: &str, symbol: &str) -> bool {
        // Fast path: check existing cached entry
        if let Some(entry) = self.filters.get(path) {
            let (ref filter, cached_mtime) = *entry;
            if cached_mtime == mtime {
                return filter.contains(symbol);
            }
        }

        // Cache miss or stale: build and cache a new filter
        let filter = build_filter(content);
        let result = filter.contains(symbol);
        self.filters.insert(path.to_path_buf(), (filter, mtime));
        result
    }
}

/// Build a Bloom filter from file content by extracting all identifiers.
fn build_filter(content: &str) -> BloomFilter {
    let idents: Vec<&str> = extract_identifiers(content).collect();
    // Sized for total token count, not unique identifiers -- duplicates over-allocate
    // the filter, so the achieved FPR is well below the 0.01 target in practice.
    let expected = idents.len().max(1);

    let mut filter = BloomFilter::with_false_pos(0.01).expected_items(expected);
    for ident in idents {
        filter.insert(ident);
    }
    filter
}

// ---------------------------------------------------------------------------
// Identifier extraction (byte-level state machine)
// ---------------------------------------------------------------------------

/// Extract identifier tokens from source code using a simple byte-level
/// state machine. Skips string literals and block/line comments.
///
/// An identifier is `[a-zA-Z_][a-zA-Z0-9_]*`.
///
/// This is intentionally approximate -- it does not understand all language
/// syntaxes perfectly, but is fast and good enough for Bloom filter population.
fn extract_identifiers(content: &str) -> impl Iterator<Item = &str> {
    IdentifierIter::new(content)
}

/// States for the identifier extraction state machine.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ScanState {
    /// Normal code scanning.
    Code,
    /// Inside a double-quoted string.
    StringDouble,
    /// Inside a single-quoted string/char.
    StringSingle,
    /// Inside a backtick string (JS template literals, Go raw strings).
    StringBacktick,
    /// Inside a line comment (// ...).
    LineComment,
    /// Inside a block comment (/* ... */).
    BlockComment,
}

struct IdentifierIter<'a> {
    bytes: &'a [u8],
    src: &'a str,
    pos: usize,
    state: ScanState,
}

impl<'a> IdentifierIter<'a> {
    fn new(content: &'a str) -> Self {
        Self {
            bytes: content.as_bytes(),
            src: content,
            pos: 0,
            state: ScanState::Code,
        }
    }
}

impl<'a> Iterator for IdentifierIter<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        let bytes = self.bytes;
        let len = bytes.len();

        while self.pos < len {
            let i = self.pos;
            let b = bytes[i];

            match self.state {
                ScanState::Code => {
                    // Check for start of string literals
                    if b == b'"' {
                        self.state = ScanState::StringDouble;
                        self.pos += 1;
                        continue;
                    }
                    if b == b'\'' {
                        self.state = ScanState::StringSingle;
                        self.pos += 1;
                        continue;
                    }
                    if b == b'`' {
                        self.state = ScanState::StringBacktick;
                        self.pos += 1;
                        continue;
                    }

                    // Check for comments
                    if b == b'/' && i + 1 < len {
                        if bytes[i + 1] == b'/' {
                            self.state = ScanState::LineComment;
                            self.pos += 2;
                            continue;
                        }
                        if bytes[i + 1] == b'*' {
                            self.state = ScanState::BlockComment;
                            self.pos += 2;
                            continue;
                        }
                    }

                    // Check for start of identifier
                    if is_ident_start(b) {
                        let start = i;
                        self.pos += 1;
                        while self.pos < len && is_ident_continue(bytes[self.pos]) {
                            self.pos += 1;
                        }
                        // Safety: identifiers are pure ASCII, so byte slicing is valid UTF-8
                        return Some(&self.src[start..self.pos]);
                    }

                    self.pos += 1;
                }

                ScanState::StringDouble => {
                    if b == b'\\' && i + 1 < len {
                        self.pos += 2; // skip escaped character
                    } else if b == b'"' {
                        self.state = ScanState::Code;
                        self.pos += 1;
                    } else {
                        self.pos += 1;
                    }
                }

                ScanState::StringSingle => {
                    if b == b'\\' && i + 1 < len {
                        self.pos += 2; // skip escaped character
                    } else if b == b'\'' {
                        self.state = ScanState::Code;
                        self.pos += 1;
                    } else {
                        self.pos += 1;
                    }
                }

                ScanState::StringBacktick => {
                    if b == b'\\' && i + 1 < len {
                        self.pos += 2;
                    } else if b == b'`' {
                        self.state = ScanState::Code;
                        self.pos += 1;
                    } else {
                        self.pos += 1;
                    }
                }

                ScanState::LineComment => {
                    if b == b'\n' {
                        self.state = ScanState::Code;
                    }
                    self.pos += 1;
                }

                ScanState::BlockComment => {
                    if b == b'*' && i + 1 < len && bytes[i + 1] == b'/' {
                        self.state = ScanState::Code;
                        self.pos += 2;
                    } else {
                        self.pos += 1;
                    }
                }
            }
        }

        None
    }
}

#[inline]
fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

#[inline]
fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_membership() {
        let mut bf = BloomFilter::with_false_pos(0.01).expected_items(100);
        bf.insert("foo");
        bf.insert("bar");
        bf.insert("baz");

        assert!(bf.contains("foo"));
        assert!(bf.contains("bar"));
        assert!(bf.contains("baz"));
    }

    #[test]
    fn test_definitely_not_present() {
        let mut bf = BloomFilter::with_false_pos(0.01).expected_items(10);
        bf.insert("alpha");
        bf.insert("beta");
        bf.insert("gamma");

        // With only 3 items in a filter sized for 10 at 1% FPR,
        // these should almost certainly return false.
        let mut false_positives = 0;
        let test_items = [
            "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa", "lambda", "mu", "nu",
            "xi", "omicron", "pi", "rho", "sigma", "tau", "upsilon", "phi", "chi", "psi", "omega",
        ];
        for item in &test_items {
            if bf.contains(item) {
                false_positives += 1;
            }
        }
        // At most 1 false positive out of 21 items is generous
        assert!(
            false_positives <= 1,
            "too many false positives: {false_positives}/{}",
            test_items.len()
        );
    }

    #[test]
    fn test_false_positive_rate() {
        let n = 500;
        let mut bf = BloomFilter::with_false_pos(0.01).expected_items(n);

        // Insert N items
        for i in 0..n {
            bf.insert(&format!("item_{i}"));
        }

        // Verify all inserted items are found
        for i in 0..n {
            assert!(bf.contains(&format!("item_{i}")), "missing item_{i}");
        }

        // Test M random items that were NOT inserted
        let m = 10_000;
        let mut false_positives = 0;
        for i in 0..m {
            if bf.contains(&format!("notinserted_{i}")) {
                false_positives += 1;
            }
        }

        let fpr = false_positives as f64 / m as f64;
        // Target is 1%, allow up to 5% for statistical variance
        assert!(
            fpr < 0.05,
            "false positive rate too high: {fpr:.4} ({false_positives}/{m})"
        );
    }

    #[test]
    fn test_identifier_extraction() {
        let code = "fn foo(bar: Baz) { qux() }";
        let idents: Vec<&str> = extract_identifiers(code).collect();
        assert_eq!(idents, vec!["fn", "foo", "bar", "Baz", "qux"]);
    }

    #[test]
    fn test_identifier_extraction_skips_strings() {
        let code = r#"let x = "hello world"; let y = 42;"#;
        let idents: Vec<&str> = extract_identifiers(code).collect();
        assert!(idents.contains(&"let"));
        assert!(idents.contains(&"x"));
        assert!(idents.contains(&"y"));
        // "hello" and "world" are inside a string -- should be skipped
        assert!(!idents.contains(&"hello"));
        assert!(!idents.contains(&"world"));
    }

    #[test]
    fn test_identifier_extraction_skips_comments() {
        let code = "fn real() // fn fake()\n/* fn also_fake() */\nfn another()";
        let idents: Vec<&str> = extract_identifiers(code).collect();
        assert!(idents.contains(&"real"));
        assert!(idents.contains(&"another"));
        assert!(!idents.contains(&"fake"));
        assert!(!idents.contains(&"also_fake"));
    }

    #[test]
    fn test_identifier_extraction_underscores_and_numbers() {
        let code = "_private __dunder var_123 _0 a1b2c3";
        let idents: Vec<&str> = extract_identifiers(code).collect();
        assert_eq!(
            idents,
            vec!["_private", "__dunder", "var_123", "_0", "a1b2c3"]
        );
    }

    #[test]
    fn test_identifier_extraction_empty() {
        let idents: Vec<&str> = extract_identifiers("").collect();
        assert!(idents.is_empty());
    }

    #[test]
    fn test_identifier_extraction_no_identifiers() {
        let idents: Vec<&str> = extract_identifiers("123 + 456 = 789").collect();
        assert!(idents.is_empty());
    }

    #[test]
    fn test_cache_mtime_invalidation() {
        let cache = BloomFilterCache::new();
        let path = Path::new("/tmp/test_bloom.rs");

        let old_content = "fn old_function() {}";
        let new_content = "fn new_function() {}";

        let mtime_old = SystemTime::UNIX_EPOCH;
        let mtime_new = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);

        // Cache with old content
        assert!(cache.contains(path, mtime_old, old_content, "old_function"));
        assert!(!cache.contains(path, mtime_old, old_content, "new_function"));

        // Same mtime: should use cached filter (old content), even though
        // we pass new content -- the cache trusts the mtime.
        assert!(cache.contains(path, mtime_old, new_content, "old_function"));

        // Different mtime: should rebuild from new content
        assert!(cache.contains(path, mtime_new, new_content, "new_function"));
        assert!(!cache.contains(path, mtime_new, new_content, "old_function"));
    }

    #[test]
    fn test_identifier_extraction_escaped_strings() {
        let code = r#"let s = "escaped \"quote\" inside"; let t = 1;"#;
        let idents: Vec<&str> = extract_identifiers(code).collect();
        assert!(idents.contains(&"s"));
        assert!(idents.contains(&"t"));
        // "quote" and "inside" are inside the string -- should be skipped
        assert!(!idents.contains(&"quote"));
        assert!(!idents.contains(&"inside"));
    }

    #[test]
    fn test_identifier_extraction_single_quotes() {
        let code = "let c = 'a'; let d = 'b';";
        let idents: Vec<&str> = extract_identifiers(code).collect();
        assert!(idents.contains(&"let"));
        assert!(idents.contains(&"c"));
        assert!(idents.contains(&"d"));
    }

    #[test]
    fn test_build_filter_integration() {
        let content = "pub fn search(query: &str) -> Vec<Match> { find(query) }";
        let filter = build_filter(content);

        assert!(filter.contains("search"));
        assert!(filter.contains("query"));
        assert!(filter.contains("Vec"));
        assert!(filter.contains("Match"));
        assert!(filter.contains("find"));
        assert!(!filter.contains("nonexistent_symbol_xyz"));
    }
}
