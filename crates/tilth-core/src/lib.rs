//! tilth-core — tilth's parsing substrate.
//!
//! Tree-sitter outlines, language detection, test-file classification, import
//! resolution, callers, and file-level dependency analysis, as a library the
//! `tilth` binary and other crates consume. The documented surface is the set
//! of items re-exported at this root; the modules behind it are the binary's
//! internals and carry no stability promise.
#![warn(clippy::pedantic)]
#![warn(missing_docs)]
#![allow(
    clippy::cast_possible_truncation,  // line numbers as u32, token counts — we target 64-bit
    clippy::cast_sign_loss,            // same
    clippy::cast_possible_wrap,        // u32→i32 for tree-sitter APIs
    clippy::module_name_repetitions,   // Rust naming conventions
    clippy::similar_names,             // common in parser/search code
    clippy::too_many_lines,            // crate-wide to cover find_definitions in src/search/symbol.rs;
                                       // narrow to a per-function allow once a refactor shrinks that file
    clippy::too_many_arguments,        // internal recursive AST walker
    clippy::unnecessary_wraps,         // Result return for API consistency
    clippy::missing_errors_doc,        // internal fns don't need error docs
    clippy::missing_panics_doc,        // same
)]

#[doc(hidden)]
pub mod cache;
#[doc(hidden)]
pub mod error;
#[doc(hidden)]
pub mod index;
#[doc(hidden)]
pub mod lang;
#[doc(hidden)]
pub mod read;
#[doc(hidden)]
pub mod search;
#[doc(hidden)]
pub mod types;

pub use error::TilthError;
pub use index::bloom::BloomFilterCache;
pub use lang::detect_file_type;
pub use lang::outline::{extract_import_source, get_outline_entries};
pub use read::imports::{is_external, is_import_line, resolve_related_files_with_content};
pub use read::outline::test_file::{test_entries, TestEntry, TestKind};
pub use search::callers::{find_callers_batch, CallerMatch};
pub use search::deps::{analyze_deps, Dependent, DepsResult, LocalDep};
pub use types::{is_test_file, FileType, Lang, OutlineEntry, OutlineKind};
