//! Materialized symbol index — pre-computes symbol-to-file mappings for O(1) resolution.
//!
//! Instead of walking the entire tree on every symbol query, `SymbolIndex::build()`
//! parses all code files in scope using tree-sitter and stores (`symbol_name` -> locations)
//! in a concurrent `DashMap`. Subsequent lookups are O(1) hash lookups plus a filter.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use dashmap::DashMap;

use crate::read::detect_file_type;
use crate::read::outline::code::outline_language;
use crate::search::treesitter::{extract_definition_name, DEFINITION_KINDS};
use crate::types::FileType;

/// Maximum file size to index (500 KB). Matches the limit in symbol search.
const MAX_FILE_SIZE: u64 = 500_000;

/// Per-file extraction result: (path, mtime, extracted symbols).
type FileSymbols = (PathBuf, SystemTime, Vec<(Arc<str>, u32, bool)>);

/// A location where a symbol appears in the codebase.
#[derive(Clone, Debug)]
pub struct SymbolLocation {
    pub path: PathBuf,
    pub line: u32,
    pub is_definition: bool,
    pub mtime: SystemTime,
}

/// Pre-computed symbol-to-file index for O(1) lookups.
///
/// Uses `DashMap` for lock-free concurrent reads and writes.
/// Keys are `Arc<str>` for memory-efficient string interning — many lookups
/// against the same symbol names benefit from shared allocations.
pub struct SymbolIndex {
    /// `symbol_name` -> list of locations
    symbols: DashMap<Arc<str>, Vec<SymbolLocation>>,
    /// file -> mtime when last indexed
    indexed_files: DashMap<PathBuf, SystemTime>,
}

impl Default for SymbolIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolIndex {
    /// Create an empty symbol index.
    #[must_use]
    pub fn new() -> Self {
        Self {
            symbols: DashMap::new(),
            indexed_files: DashMap::new(),
        }
    }

    /// Build the index by walking all code files in `scope`.
    ///
    /// Uses `ignore::WalkBuilder` with the same directory filtering as search
    /// (skipping `.git`, `node_modules`, `target`, etc.) and processes files
    /// in parallel via rayon for speed.
    pub fn build(&self, scope: &Path) {
        use ignore::WalkBuilder;
        use rayon::prelude::*;

        // Collect file paths first, then process in parallel with rayon.
        // We use WalkBuilder for directory filtering but rayon for parallelism
        // because rayon gives us better work-stealing than ignore's parallel walker
        // for CPU-bound tree-sitter parsing.
        let files: Vec<PathBuf> = WalkBuilder::new(scope)
            .hidden(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .ignore(false)
            .parents(false)
            .filter_entry(|entry| {
                if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                    if let Some(name) = entry.file_name().to_str() {
                        return !crate::search::SKIP_DIRS.contains(&name);
                    }
                }
                true
            })
            .build()
            .filter_map(|entry| {
                let entry = entry.ok()?;
                if !entry.file_type()?.is_file() {
                    return None;
                }
                let path = entry.into_path();
                // Only index code files that have tree-sitter grammars
                if let FileType::Code(lang) = detect_file_type(&path) {
                    if outline_language(lang).is_some() {
                        // Skip oversized files
                        if let Ok(meta) = fs::metadata(&path) {
                            if meta.len() <= MAX_FILE_SIZE {
                                return Some(path);
                            }
                        }
                    }
                }
                None
            })
            .collect();

        // Process files in parallel with rayon
        let results: Vec<FileSymbols> = files
            .par_iter()
            .filter_map(|path| {
                let content = fs::read_to_string(path).ok()?;
                let mtime = fs::metadata(path)
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                let symbols = extract_symbols(path, &content);
                if symbols.is_empty() {
                    // Still record the file as indexed even if no symbols found
                    Some((path.clone(), mtime, Vec::new()))
                } else {
                    Some((path.clone(), mtime, symbols))
                }
            })
            .collect();

        // Insert results into the DashMaps
        for (path, mtime, symbols) in results {
            self.indexed_files.insert(path.clone(), mtime);
            for (name, line, is_def) in symbols {
                let loc = SymbolLocation {
                    path: path.clone(),
                    line,
                    is_definition: is_def,
                    mtime,
                };
                self.symbols.entry(name).or_default().push(loc);
            }
        }
    }

    /// Check if the index has been built for the given scope.
    ///
    /// Simple heuristic: returns true if any indexed file path starts with `scope`.
    #[must_use]
    pub fn is_built(&self, scope: &Path) -> bool {
        self.indexed_files
            .iter()
            .any(|entry| entry.key().starts_with(scope))
    }

    /// Look up all locations of a symbol within `scope`.
    ///
    /// Returns matching locations filtered to paths within `scope`.
    /// Results may include stale entries if files have changed since indexing --
    /// callers can check `mtime` against the current file if freshness matters.
    #[must_use]
    pub fn lookup(&self, name: &str, scope: &Path) -> Vec<SymbolLocation> {
        let key: Arc<str> = Arc::from(name);
        let Some(locations) = self.symbols.get(&key) else {
            return Vec::new();
        };
        locations
            .iter()
            .filter(|loc| loc.path.starts_with(scope))
            .cloned()
            .collect()
    }

    /// Look up only definition locations of a symbol within `scope`.
    ///
    /// Same as `lookup` but filters to `is_definition == true`.
    #[must_use]
    pub fn lookup_definitions(&self, name: &str, scope: &Path) -> Vec<SymbolLocation> {
        let key: Arc<str> = Arc::from(name);
        let Some(locations) = self.symbols.get(&key) else {
            return Vec::new();
        };
        locations
            .iter()
            .filter(|loc| loc.is_definition && loc.path.starts_with(scope))
            .cloned()
            .collect()
    }

    /// Index a single file, updating the symbol maps.
    ///
    /// Used for incremental updates when a file changes.
    /// Removes old entries for this file before inserting new ones.
    pub fn index_file(&self, path: &Path, content: &str) {
        let mtime = fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        // Remove old entries for this file from all symbol lists
        let old_mtime = self.indexed_files.get(path).map(|r| *r.value());
        if old_mtime.is_some() {
            self.symbols.iter_mut().for_each(|mut entry| {
                entry.value_mut().retain(|loc| loc.path != path);
            });
        }

        // Extract and insert new symbols
        let symbols = extract_symbols(path, content);
        self.indexed_files.insert(path.to_path_buf(), mtime);

        for (name, line, is_def) in symbols {
            let loc = SymbolLocation {
                path: path.to_path_buf(),
                line,
                is_definition: is_def,
                mtime,
            };
            self.symbols.entry(name).or_default().push(loc);
        }
    }

    /// Number of unique symbol names in the index.
    #[must_use]
    pub fn symbol_count(&self) -> usize {
        self.symbols.len()
    }

    /// Number of indexed files.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.indexed_files.len()
    }
}

/// Extract all symbol definitions from a file using tree-sitter.
///
/// Returns a list of `(name, line_number, is_definition)` tuples.
/// Line numbers are 1-based (matching the convention used in search results).
///
/// Only extracts definitions (function, struct, trait, class, etc.) --
/// not usages. This keeps the index focused and compact.
fn extract_symbols(path: &Path, content: &str) -> Vec<(Arc<str>, u32, bool)> {
    let FileType::Code(lang) = detect_file_type(path) else {
        return Vec::new();
    };

    let Some(ts_lang) = outline_language(lang) else {
        return Vec::new();
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return Vec::new();
    }

    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };

    let lines: Vec<&str> = content.lines().collect();
    let mut symbols = Vec::new();

    walk_definitions(tree.root_node(), &lines, &mut symbols, 0);

    symbols
}

/// Recursively walk tree-sitter AST nodes to find all definitions.
///
/// Unlike `search::symbol::walk_for_definitions` which searches for a specific name,
/// this extracts ALL definition names for index building.
/// Depth-limited to 3 levels (matching search behavior) to avoid descending
/// into deeply nested anonymous blocks.
fn walk_definitions(
    node: tree_sitter::Node,
    lines: &[&str],
    symbols: &mut Vec<(Arc<str>, u32, bool)>,
    depth: usize,
) {
    if depth > 3 {
        return;
    }

    let kind = node.kind();

    if DEFINITION_KINDS.contains(&kind) {
        if let Some(name) = extract_definition_name(node, lines) {
            let line = node.start_position().row as u32 + 1;
            symbols.push((Arc::from(name.as_str()), line, true));
        }

        // For impl blocks in Rust, also index the trait name and type name
        // so lookups for "MyTrait" find `impl MyTrait for Foo`.
        if kind == "impl_item" {
            if let Some(trait_name) = crate::search::treesitter::extract_impl_trait(node, lines) {
                let line = node.start_position().row as u32 + 1;
                symbols.push((Arc::from(trait_name.as_str()), line, true));
            }
            if let Some(type_name) = crate::search::treesitter::extract_impl_type(node, lines) {
                let line = node.start_position().row as u32 + 1;
                symbols.push((Arc::from(type_name.as_str()), line, true));
            }
        }

        // For classes implementing interfaces, index the interface names too
        if kind == "class_declaration" || kind == "class_definition" {
            let interfaces = crate::search::treesitter::extract_implemented_interfaces(node, lines);
            for iface in interfaces {
                let line = node.start_position().row as u32 + 1;
                symbols.push((Arc::from(iface.as_str()), line, true));
            }
        }
    }

    // Recurse into children for nested definitions (impl blocks, class bodies, modules)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_definitions(child, lines, symbols, depth + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_empty_index() {
        let index = SymbolIndex::new();
        assert_eq!(index.symbol_count(), 0);
        assert_eq!(index.file_count(), 0);
        assert!(!index.is_built(Path::new("/tmp")));
        assert!(index.lookup("foo", Path::new("/tmp")).is_empty());
    }

    #[test]
    fn test_extract_symbols_rust() {
        let content = r#"
pub struct Foo {
    bar: u32,
}

impl Foo {
    pub fn baz(&self) -> u32 {
        self.bar
    }
}

trait MyTrait {
    fn do_thing(&self);
}

impl MyTrait for Foo {
    fn do_thing(&self) {}
}
"#;
        let dir = std::env::temp_dir().join("tilth_test_extract_symbols");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("test.rs");
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();

        let symbols = extract_symbols(&path, content);
        let names: Vec<&str> = symbols.iter().map(|(n, _, _)| n.as_ref()).collect();

        assert!(names.contains(&"Foo"), "should find struct Foo: {names:?}");
        assert!(names.contains(&"baz"), "should find fn baz: {names:?}");
        assert!(
            names.contains(&"MyTrait"),
            "should find trait MyTrait: {names:?}"
        );
        assert!(
            names.contains(&"do_thing"),
            "should find fn do_thing: {names:?}"
        );

        // All extracted symbols should be definitions
        assert!(symbols.iter().all(|(_, _, is_def)| *is_def));

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_index_file() {
        let content = "pub fn hello() {}\npub fn world() {}";
        let dir = std::env::temp_dir().join("tilth_test_index_file");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("test.rs");
        fs::write(&path, content).unwrap();

        let index = SymbolIndex::new();
        index.index_file(&path, content);

        assert_eq!(index.file_count(), 1);
        let results = index.lookup("hello", &dir);
        assert_eq!(results.len(), 1);
        assert!(results[0].is_definition);
        assert_eq!(results[0].line, 1);

        let results = index.lookup("world", &dir);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].line, 2);

        // Test incremental update
        let new_content = "pub fn hello() {}\npub fn updated() {}";
        fs::write(&path, new_content).unwrap();
        index.index_file(&path, new_content);

        // "world" should be gone, "updated" should be present
        assert!(index.lookup("world", &dir).is_empty());
        assert_eq!(index.lookup("updated", &dir).len(), 1);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_lookup_definitions_filter() {
        let content = "pub fn target() {}";
        let dir = std::env::temp_dir().join("tilth_test_lookup_defs");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("test.rs");
        fs::write(&path, content).unwrap();

        let index = SymbolIndex::new();
        index.index_file(&path, content);

        let defs = index.lookup_definitions("target", &dir);
        assert_eq!(defs.len(), 1);
        assert!(defs[0].is_definition);

        // lookup_definitions with wrong scope should return empty
        let defs = index.lookup_definitions("target", Path::new("/nonexistent"));
        assert!(defs.is_empty());

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_extract_symbols_typescript() {
        let content = r#"
function greet(name: string): string {
    return `Hello, ${name}!`;
}

class Greeter {
    greeting: string;
    constructor(message: string) {
        this.greeting = message;
    }
}

interface Printable {
    print(): void;
}
"#;
        let dir = std::env::temp_dir().join("tilth_test_extract_ts");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("test.ts");
        fs::write(&path, content).unwrap();

        let symbols = extract_symbols(&path, content);
        let names: Vec<&str> = symbols.iter().map(|(n, _, _)| n.as_ref()).collect();

        assert!(
            names.contains(&"greet"),
            "should find function greet: {names:?}"
        );
        assert!(
            names.contains(&"Greeter"),
            "should find class Greeter: {names:?}"
        );
        assert!(
            names.contains(&"Printable"),
            "should find interface Printable: {names:?}"
        );

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_extract_symbols_python() {
        let content = r#"
def hello():
    pass

class MyClass:
    def method(self):
        pass
"#;
        let dir = std::env::temp_dir().join("tilth_test_extract_py");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("test.py");
        fs::write(&path, content).unwrap();

        let symbols = extract_symbols(&path, content);
        let names: Vec<&str> = symbols.iter().map(|(n, _, _)| n.as_ref()).collect();

        assert!(names.contains(&"hello"), "should find def hello: {names:?}");
        assert!(
            names.contains(&"MyClass"),
            "should find class MyClass: {names:?}"
        );

        let _ = fs::remove_file(&path);
    }
}
