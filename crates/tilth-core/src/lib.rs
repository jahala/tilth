//! tilth-core is the parsing substrate under the `tilth` binary.
//!
//! It detects a file's language, parses it with tree-sitter into outline
//! entries and test structure, resolves imports, finds call sites, and
//! analyses file-level dependencies. Everything it returns is data; the
//! `tilth` binary turns that data into the text its CLI and MCP tools print.
//!
//! The documented surface is the set of items re-exported at this root. The
//! modules behind it are shared with the `tilth` binary and carry no
//! stability promise.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[doc(hidden)]
pub mod cache;
#[doc(hidden)]
pub mod error;
#[doc(hidden)]
pub mod imports;
#[doc(hidden)]
pub mod index;
#[doc(hidden)]
pub mod lang;
#[doc(hidden)]
pub mod search;
#[doc(hidden)]
pub mod source;
#[cfg(any(test, feature = "test-fixtures"))]
#[doc(hidden)]
pub mod test_readers;
#[doc(hidden)]
pub mod test_structure;
#[doc(hidden)]
pub mod types;

pub use error::TilthError;
pub use imports::{
    extract_imports, is_external, is_import_line, resolve_related_files_with_content, Import,
};
pub use index::bloom::BloomFilterCache;
pub use lang::outline::{extract_import_source, get_outline_entries};
pub use lang::{detect_file_type, detect_file_type_with_content};
pub use search::callers::{find_callers_batch, CallerMatch, CallerWalk};
pub use search::deps::{analyze_deps, Dependent, DepsResult, LocalDep};
pub use test_structure::{test_entries, TestEntry, TestKind};
pub use types::{is_test_file, FileType, Lang, OutlineEntry, OutlineKind};
