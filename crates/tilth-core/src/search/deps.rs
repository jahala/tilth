//! File-level dependency analysis: what a file imports and what imports it.
//! `analyze_deps` returns the data; the `tilth` binary renders it for
//! `tilth_deps`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::error::TilthError;
use crate::imports::{extract_imports, is_external, resolve_related_files_with_content};
use crate::lang::detect_file_type_with_content;
use crate::lang::outline::get_outline_entries;
use crate::search::callees::{extract_callee_names, resolve_callees};
use crate::search::callers::find_callers_batch;
use crate::types::{is_test_file, FileType, OutlineKind};

/// Maximum number of exported symbols to search for in the reverse direction.
pub const MAX_EXPORTED_SYMBOLS: usize = 25;

/// Result of a full dependency analysis for a single file.
#[derive(Debug)]
pub struct DepsResult {
    pub target: PathBuf,
    pub uses_local: Vec<LocalDep>,
    pub uses_external: Vec<String>,
    /// Every file that calls one of the target's symbols, same directory
    /// first, then non-tests before tests, then by path.
    pub used_by: Vec<Dependent>,
    /// Code files skipped for size although they name one of the target's
    /// symbols, so they may be dependents nobody checked.
    pub skipped_large: Vec<PathBuf>,
    pub exported_count: usize,
    /// Actual number of symbols searched (may be < `exported_count` if capped).
    pub searched_count: usize,
}

/// A local file dependency with the symbols used from it.
#[derive(Debug)]
pub struct LocalDep {
    pub path: PathBuf,
    pub symbols: Vec<String>,
}

/// A file that depends on the target, with symbol-level call detail.
#[derive(Debug)]
pub struct Dependent {
    pub path: PathBuf,
    /// (`calling_function`, `called_symbol`, `line`) triples.
    pub symbols: Vec<(String, String, u32)>,
    pub is_test: bool,
}

/// Analyse the dependency graph for `path` within `scope`.
///
/// Phase 1: Extract exported symbols from the outline.
/// Phase 2: Forward dependencies — what this file uses.
/// Phase 3: Reverse dependencies — what uses this file.
pub fn analyze_deps(
    path: &Path,
    scope: &Path,
    bloom: &crate::index::bloom::BloomFilterCache,
) -> Result<DepsResult, TilthError> {
    // Canonicalize for reliable path comparison (callers return absolute paths).
    let path = &path.canonicalize().map_err(|e| TilthError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;

    let content = crate::source::read_source(path).map_err(|e| TilthError::IoError {
        path: path.clone(),
        source: e,
    })?;

    let FileType::Code(lang) = detect_file_type_with_content(path, &content) else {
        // Non-code file: return empty deps gracefully.
        return Ok(DepsResult {
            target: path.clone(),
            uses_local: Vec::new(),
            uses_external: Vec::new(),
            used_by: Vec::new(),
            skipped_large: Vec::new(),
            exported_count: 0,
            searched_count: 0,
        });
    };

    // ── Phase 1: Extract exported symbols ────────────────────────────────────

    let entries = get_outline_entries(&content, lang);

    let mut all_names: Vec<String> = Vec::new();
    for entry in &entries {
        // Skip imports and re-export wrappers — they don't define symbols here.
        if matches!(entry.kind, OutlineKind::Import | OutlineKind::Export) {
            continue;
        }
        collect_symbol_names(entry, &mut all_names);
    }

    // Deduplicate
    all_names.sort();
    all_names.dedup();

    // Filter placeholder / noise names
    all_names.retain(|n| !is_placeholder_name(n));

    let exported_count = all_names.len();

    // Cap at MAX_EXPORTED_SYMBOLS, preferring longer (more specific) names
    let searched_count = if all_names.len() > MAX_EXPORTED_SYMBOLS {
        all_names.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        all_names.truncate(MAX_EXPORTED_SYMBOLS);
        MAX_EXPORTED_SYMBOLS
    } else {
        all_names.len()
    };

    // ── Phase 2: Forward dependencies ────────────────────────────────────────

    // Local deps via callee resolution
    let callee_names = extract_callee_names(&content, lang, None);
    let resolved = resolve_callees(&callee_names, path, &content, bloom);

    // Group resolved callees by file
    let mut local_by_file: HashMap<PathBuf, Vec<String>> = HashMap::new();
    for callee in resolved {
        if callee.file != *path {
            local_by_file
                .entry(callee.file)
                .or_default()
                .push(callee.name);
        }
    }

    // Merge in import-resolved files (may not have resolved callees if symbols
    // weren't matched, but the import relationship itself is meaningful)
    let import_files = resolve_related_files_with_content(path, &content);
    for import_path in import_files {
        local_by_file.entry(import_path).or_default();
    }

    // Sort symbols within each dep, then build the list sorted by path
    let mut uses_local: Vec<LocalDep> = local_by_file
        .into_iter()
        .map(|(dep_path, mut syms)| {
            syms.sort();
            syms.dedup();
            LocalDep {
                path: dep_path,
                symbols: syms,
            }
        })
        .collect();
    uses_local.sort_by(|a, b| a.path.cmp(&b.path));

    // External deps: one per module the imports name
    let mut external_set: HashSet<String> = HashSet::new();
    for import in extract_imports(&content, lang) {
        let source = import.source;
        if is_external(&source, lang) && !is_stdlib(&source, lang) && is_valid_module_path(&source)
        {
            external_set.insert(source);
        }
    }
    let mut uses_external: Vec<String> = external_set.into_iter().collect();
    uses_external.sort();

    // ── Phase 3: Reverse dependencies ────────────────────────────────────────

    let (used_by, skipped_large) = if searched_count > 0 {
        let symbols_set: HashSet<String> = all_names.iter().cloned().collect();
        // One `Dependent` per file that calls the target's symbols. Calls from
        // within the target file itself are self-references, not dependents.
        let found = find_callers_batch(&symbols_set, scope, bloom, None, None, |sites| {
            let dep_path = sites.first().map(|(_, m)| m.path.clone())?;
            if dep_path == *path {
                return None;
            }
            let mut pairs: Vec<(String, String, u32)> = sites
                .into_iter()
                .map(|(symbol, m)| (m.calling_function, symbol, m.line))
                .collect();
            pairs.sort();
            pairs.dedup();
            let is_test = is_test_file(&dep_path);
            Some(Dependent {
                path: dep_path,
                symbols: pairs,
                is_test,
            })
        })?;

        let target_dir = path.parent();
        let mut dependents: Vec<Dependent> = found.per_file.into_iter().flatten().collect();

        // Sort: same directory first, non-tests before tests, then alphabetical
        dependents.sort_by(|a, b| {
            let a_same_dir = target_dir.is_some_and(|d| a.path.parent() == Some(d));
            let b_same_dir = target_dir.is_some_and(|d| b.path.parent() == Some(d));
            b_same_dir
                .cmp(&a_same_dir)
                .then_with(|| a.is_test.cmp(&b.is_test))
                .then_with(|| a.path.cmp(&b.path))
        });

        (dependents, found.skipped_large)
    } else {
        (Vec::new(), Vec::new())
    };

    Ok(DepsResult {
        target: path.clone(),
        uses_local,
        uses_external,
        used_by,
        skipped_large,
        exported_count,
        searched_count,
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Collect symbol names from an outline entry and its children.
fn collect_symbol_names(entry: &crate::types::OutlineEntry, out: &mut Vec<String>) {
    out.push(entry.name.clone());
    for child in &entry.children {
        // Include public methods of classes/structs/impls
        if !matches!(child.kind, OutlineKind::Import | OutlineKind::Export) {
            out.push(child.name.clone());
        }
    }
}

/// Returns true if the name is a noise/placeholder that should be excluded
/// from the reverse-dependency search.
fn is_placeholder_name(name: &str) -> bool {
    if name == "<anonymous>" {
        return true;
    }
    if name.starts_with('<') {
        return true;
    }
    if name.starts_with("impl ") {
        return true;
    }
    // Single-character names are too generic (e.g. `T`, `E`, `f`)
    if name.chars().count() == 1 {
        return true;
    }
    false
}

/// Returns true if the import source is a standard library module.
/// Agents can't navigate into stdlib — showing these is noise.
fn is_stdlib(source: &str, lang: crate::types::Lang) -> bool {
    crate::lang::spec::spec(lang).stdlib.matches(source)
}

/// Returns true if the string looks like a valid module/package path.
/// Filters out what a line-read import yields that names no module.
fn is_valid_module_path(source: &str) -> bool {
    // Must not contain spaces (real module paths don't)
    if source.contains(' ') {
        return false;
    }
    // Must start with an alphanumeric, @, or dot
    source
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '@' || c == '.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    use std::fs;

    /// The external modules `analyze_deps` reports for one file.
    fn externals_of(file: &str, content: &str) -> Vec<String> {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(file);
        fs::write(&path, content).unwrap();
        let bloom = crate::index::bloom::BloomFilterCache::new();
        analyze_deps(&path, tmp.path(), &bloom)
            .unwrap()
            .uses_external
    }

    /// A UTF-8 byte-order mark sits in front of line 1, so a file that opens
    /// with an import lost it: `\u{feff}#include "b.h"` is not an include.
    #[test]
    fn a_byte_order_mark_keeps_the_first_import() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("b.h"), "int b(void);\n").unwrap();
        fs::write(tmp.path().join("c.h"), "int c(void);\n").unwrap();
        let a = tmp.path().join("a.cpp");
        fs::write(
            &a,
            "\u{feff}#include \"b.h\"\n#include \"c.h\"\nint a() { return b() + c(); }\n",
        )
        .unwrap();
        let bloom = crate::index::bloom::BloomFilterCache::new();
        let deps = analyze_deps(&a, tmp.path(), &bloom).unwrap();
        let mut local: Vec<String> = deps
            .uses_local
            .iter()
            .map(|d| d.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        local.sort();
        assert_eq!(local, ["b.h", "c.h"]);

        let py = tmp.path().join("m.py");
        fs::write(&py, "\u{feff}import requests\nimport numpy\n").unwrap();
        assert_eq!(
            analyze_deps(&py, tmp.path(), &bloom).unwrap().uses_external,
            ["numpy", "requests"]
        );
    }

    /// Each spec of a Go import block is its own module. Read line by line,
    /// the block's first line was `import (` and the specs were not imports
    /// at all, so a file importing only through a block used nothing.
    #[test]
    fn go_import_block_names_each_module() {
        let content = "package m\n\nimport (\n\t\"fmt\"\n\tx \"example.com/m/core\"\n\t_ \"example.com/m/seams\"\n)\n";
        assert_eq!(
            externals_of("main.go", content),
            ["example.com/m/core", "example.com/m/seams"]
        );
    }

    /// `import requests, numpy` names two modules. Read as one, the source
    /// was `requests, numpy`, which is no module path, so both were dropped,
    /// and so was the aliased `yaml as y`.
    #[test]
    fn python_import_list_names_each_module() {
        let content = "import requests, numpy\nimport yaml as y\n";
        assert_eq!(
            externals_of("main.py", content),
            ["numpy", "requests", "yaml"]
        );
    }

    /// A Rust `use` group names one module per member. Read as one, the
    /// source was `serde` and the members were gone; an alias kept its
    /// `as` clause and was then dropped as not a module path.
    #[test]
    fn rust_use_group_names_each_member() {
        let content = "use serde::{Deserialize, de::Visitor};\nuse serde_json::Value as Json;\n";
        assert_eq!(
            externals_of("lib.rs", content),
            [
                "serde::Deserialize",
                "serde::de::Visitor",
                "serde_json::Value"
            ]
        );
    }

    /// Scala's `import cats.{Functor, Monad}` names two modules. Read as one,
    /// the source held a space and was dropped.
    #[test]
    fn scala_import_selectors_name_each_module() {
        let content = "import cats.{Functor, Monad => M}\nimport akka.actor._\n";
        assert_eq!(
            externals_of("Main.scala", content),
            ["akka.actor._", "cats.Functor", "cats.Monad"]
        );
    }

    /// Elixir's `alias Foo.{Bar, Baz}` names two modules. Read as one, the
    /// source was `Foo.{Bar`.
    #[test]
    fn elixir_alias_group_names_each_module() {
        let content =
            "defmodule M do\n  alias Foo.{Bar, Baz}\n  import Ecto.Query, only: [from: 2]\nend\n";
        assert_eq!(
            externals_of("m.ex", content),
            ["Ecto.Query", "Foo.Bar", "Foo.Baz"]
        );
    }

    #[test]
    fn go_stdlib_fmt_is_stdlib() {
        assert!(is_stdlib("fmt", crate::types::Lang::Go));
    }

    #[test]
    fn go_stdlib_fmtlib_is_not_stdlib() {
        // "fmtlib" is not a Go stdlib package—previously matched via starts_with("fmt")
        assert!(!is_stdlib("fmtlib", crate::types::Lang::Go));
    }

    #[test]
    fn go_stdlib_fmtutil_is_not_stdlib() {
        assert!(!is_stdlib("fmtutil", crate::types::Lang::Go));
    }

    #[test]
    fn go_stdlib_multi_segment_paths_are_stdlib() {
        // Regression: multi-segment stdlib imports (single-line form
        // `import "net/http"`) must classify as stdlib via their root segment.
        // The exact-match allowlist briefly regressed these to "external".
        for path in [
            "net/http",
            "encoding/json",
            "path/filepath",
            "crypto/sha256",
            "text/template",
            "container/list",
            "database/sql",
        ] {
            assert!(
                is_stdlib(path, crate::types::Lang::Go),
                "{path} should be classified as Go stdlib"
            );
        }
    }

    #[test]
    fn go_local_multi_segment_path_is_not_stdlib() {
        // A local/third-party multi-segment package whose root isn't stdlib.
        assert!(!is_stdlib("mypackage/sub", crate::types::Lang::Go));
    }

    #[test]
    fn go_local_package_without_dot_is_not_stdlib() {
        // A local package like "mypackage" has no dot but is NOT stdlib—
        // the old !source.contains('.') rule wrongly classified it as stdlib.
        assert!(!is_stdlib("mypackage", crate::types::Lang::Go));
    }

    #[test]
    fn go_external_dotted_path_is_not_stdlib() {
        assert!(!is_stdlib(
            "github.com/gin-gonic/gin",
            crate::types::Lang::Go
        ));
    }

    #[test]
    fn go_stdlib_cmp_and_maps_are_stdlib() {
        // Go 1.21+ added `cmp` and `maps` to the standard library.
        assert!(is_stdlib("cmp", crate::types::Lang::Go));
        assert!(is_stdlib("maps", crate::types::Lang::Go));
    }

    /// The import resolver stopped at eight files, a limit meant for the
    /// read hint, so a file importing twelve local modules reported
    /// "8 local". Sixty importers are also more than the old caller walk
    /// read before it quit on its shared counter.
    #[test]
    fn deps_counts_every_local_import_and_every_dependent() {
        let dir = tempfile::tempdir().unwrap();
        let mut target = String::new();
        for i in 0..12 {
            let _ = writeln!(target, "import {{ m{i} }} from './m{i}';");
            fs::write(
                dir.path().join(format!("m{i}.ts")),
                format!("export function m{i}() {{ return {i}; }}\n"),
            )
            .unwrap();
        }
        target.push_str("export function useAll() { return m0() + m11(); }\n");
        fs::write(dir.path().join("target.ts"), target).unwrap();
        for i in 0..60 {
            fs::write(
                dir.path().join(format!("user{i:02}.ts")),
                format!(
                    "import {{ useAll }} from './target';\nexport function u{i}() {{ return useAll(); }}\n"
                ),
            )
            .unwrap();
        }
        let bloom = crate::index::bloom::BloomFilterCache::new();
        let result = analyze_deps(&dir.path().join("target.ts"), dir.path(), &bloom).unwrap();
        assert_eq!(result.uses_local.len(), 12, "{:?}", result.uses_local);
        assert_eq!(result.used_by.len(), 60);
        assert!(result.used_by[0].path.ends_with("user00.ts"));
    }
}
