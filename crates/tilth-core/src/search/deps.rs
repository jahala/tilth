//! File-level dependency analysis: what a file imports and what imports it.
//! `analyze_deps` returns the data; the `tilth` binary renders it for
//! `tilth_deps`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use std::sync::Mutex;

use crate::error::TilthError;
use crate::imports::{extract_imports, is_external, resolve_import};
use crate::lang::detect_file_type_with_content;
use crate::lang::outline::get_outline_entries;
use crate::search::callees::{extract_callee_names, resolve_callees};
use crate::search::callers::find_callers_batch;
use crate::types::{is_test_file, FileType, Lang, OutlineKind};

/// How many of a file's names the call-site search looks for. Each name
/// widens the walk's pre-filter, and a file with hundreds of names would
/// parse most of the tree. Past this many, the names other files import
/// by name come first, the most imported first; then the longest. A short
/// name (`new`, `fmt`) is one other files define too, so its calls say
/// least about this file.
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
    /// The names left out of the call-site search, because the file has
    /// more than `MAX_EXPORTED_SYMBOLS` (see there for the order). Empty
    /// when every name was searched.
    pub unsearched: Vec<String>,
}

/// A local file dependency with the symbols used from it.
#[derive(Debug)]
pub struct LocalDep {
    pub path: PathBuf,
    pub symbols: Vec<String>,
}

/// A file that depends on the target: it calls one of the target's names,
/// imports the target, or both.
#[derive(Debug)]
pub struct Dependent {
    pub path: PathBuf,
    /// (`calling_function`, `called_symbol`, `line`) triples.
    pub symbols: Vec<(String, String, u32)>,
    pub is_test: bool,
    /// The first lines of this file's import statements that name the
    /// target, or name `via`. Empty when the file only calls the target.
    pub import_lines: Vec<u32>,
    /// The file this one imports the target through: a file that
    /// re-exports it (`index.ts`, `__init__.py`, `export … from`). `None`
    /// when the file imports the target itself, or only calls it.
    pub via: Option<PathBuf>,
}

/// Analyse the dependency graph for `path` within `scope`.
///
/// Phase 1: Extract exported symbols from the outline.
/// Phase 2: Forward dependencies — what this file uses.
/// Phase 3: Reverse dependencies — what uses this file.
#[expect(
    clippy::too_many_lines,
    reason = "split by responsibility in architecture step 10"
)]
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
            unsearched: Vec::new(),
        });
    };

    // ── Phase 1: Extract exported symbols ────────────────────────────────────

    let entries = get_outline_entries(&content, lang);

    let mut all_names: Vec<String> = Vec::new();
    for entry in &entries {
        match entry.kind {
            OutlineKind::Import => {}
            // A clause specifier is a name other files import from here,
            // under its exported name: `export { readPlan as parsePlan }`
            // is used as `parsePlan`. Other export entries (`export * from`,
            // `export default <expr>`) name nothing.
            OutlineKind::Export => {
                if entry.signature.is_some() && is_identifier(&entry.name) {
                    all_names.push(entry.name.clone());
                }
            }
            _ => collect_symbol_names(entry, &mut all_names),
        }
    }

    // Deduplicate
    all_names.sort();
    all_names.dedup();

    // Filter placeholder / noise names
    all_names.retain(|n| !is_placeholder_name(n));

    let exported_count = all_names.len();

    // The files that import this one. What their import statements name
    // is what other files use, so past the cap those names are searched
    // first.
    let direct = importers_of(std::slice::from_ref(path), scope, &|_| true, false)?;
    let (searched, unsearched) = choose_names(all_names, &direct);

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
    let imports = extract_imports(&content, lang);
    let mut external_set: HashSet<String> = HashSet::new();
    for import in &imports {
        if let Some(import_path) = resolve_import(path, lang, import) {
            local_by_file.entry(import_path).or_default();
        } else if is_external(&import.source, lang)
            && !is_stdlib(&import.source, lang)
            && is_valid_module_path(&import.source)
        {
            external_set.insert(import.source.clone());
        }
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

    // External deps: one per module the imports name that is not local
    let mut uses_external: Vec<String> = external_set.into_iter().collect();
    uses_external.sort();

    // ── Phase 3: Reverse dependencies ────────────────────────────────────────

    let (mut found_by_path, skipped_large) = if searched.is_empty() {
        (HashMap::new(), Vec::new())
    } else {
        let symbols_set: HashSet<String> = searched.iter().cloned().collect();
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
                import_lines: Vec::new(),
                via: None,
            })
        })?;
        let by_path: HashMap<PathBuf, Dependent> = found
            .per_file
            .into_iter()
            .flatten()
            .map(|d| (d.path.clone(), d))
            .collect();
        (by_path, found.skipped_large)
    };

    // Files that import the target, whether or not they call it, and the
    // files that import it through a re-exporting file. The names keep the
    // second kind honest: a file that imports a barrel for something else
    // does not depend on the target.
    let mut every_name = searched;
    every_name.extend(unsearched.iter().cloned());
    for importer in with_reexport_importers(path, direct, scope, &every_name)? {
        let dependent = found_by_path
            .entry(importer.path.clone())
            .or_insert_with(|| Dependent {
                is_test: is_test_file(&importer.path),
                path: importer.path,
                symbols: Vec::new(),
                import_lines: Vec::new(),
                via: None,
            });
        dependent.import_lines.extend(importer.lines);
        dependent.import_lines.sort_unstable();
        dependent.import_lines.dedup();
        if dependent.via.is_none() {
            dependent.via = importer.via;
        }
    }

    let target_dir = path.parent();
    let mut used_by: Vec<Dependent> = found_by_path.into_values().collect();
    // Sort: same directory first, non-tests before tests, then alphabetical
    used_by.sort_by(|a, b| {
        let a_same_dir = target_dir.is_some_and(|d| a.path.parent() == Some(d));
        let b_same_dir = target_dir.is_some_and(|d| b.path.parent() == Some(d));
        b_same_dir
            .cmp(&a_same_dir)
            .then_with(|| a.is_test.cmp(&b.is_test))
            .then_with(|| a.path.cmp(&b.path))
    });

    Ok(DepsResult {
        target: path.clone(),
        uses_local,
        uses_external,
        used_by,
        skipped_large,
        exported_count,
        unsearched,
    })
}

/// A file found to import one of the files it was searched for.
struct Importer {
    path: PathBuf,
    /// First lines of the statements that name the file.
    lines: Vec<u32>,
    /// The text of those statements, which names what the file imports.
    statements: String,
    /// The re-exporting file the import goes through, `None` for the target.
    via: Option<PathBuf>,
    /// The file re-exports what it imports, so its own importers depend on
    /// the target too.
    reexports: bool,
}

/// The names to search for call sites, and the ones left out: all of
/// them up to `MAX_EXPORTED_SYMBOLS`; past it, the names the most
/// `importers` name in their import statements, then the longest. Ranked
/// by how many files mention a name, in the whole tree or in the
/// importers' text, the generic names won: `new`, `default` and `build`
/// took ripgrep's searcher from 14 dependents to 68, nearly all of them
/// false. An import statement names what a file takes from this one.
fn choose_names(names: Vec<String>, importers: &[Importer]) -> (Vec<String>, Vec<String>) {
    if names.len() <= MAX_EXPORTED_SYMBOLS {
        return (names, Vec::new());
    }
    let mut ranked: Vec<(usize, String)> = names
        .into_iter()
        .map(|name| {
            let imported_by = importers
                .iter()
                .filter(|i| mentions(&i.statements, &name))
                .count();
            (imported_by, name)
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.len().cmp(&a.1.len()))
            .then_with(|| a.1.cmp(&b.1))
    });
    let mut searched: Vec<String> = ranked.into_iter().map(|(_, name)| name).collect();
    let unsearched = searched.split_off(MAX_EXPORTED_SYMBOLS);
    (searched, unsearched)
}

/// `direct`, the files that import `target`, followed by every file that
/// imports one of those that re-export it by a statement naming one of
/// `names`, and so on up the chain of re-exporting files. Each file once.
fn with_reexport_importers(
    target: &Path,
    direct: Vec<Importer>,
    scope: &Path,
    names: &[String],
) -> Result<Vec<Importer>, TilthError> {
    let mut seen: HashSet<PathBuf> = HashSet::from([target.to_path_buf()]);
    let mut out = Vec::new();
    let mut barrels = Vec::new();
    for importer in direct {
        let canonical = importer
            .path
            .canonicalize()
            .unwrap_or_else(|_| importer.path.clone());
        if !seen.insert(canonical.clone()) {
            continue;
        }
        if importer.reexports {
            barrels.push(canonical);
        }
        out.push(importer);
    }
    let names_one = |text: &str| names.iter().any(|n| mentions(text, n));
    while !barrels.is_empty() {
        let found = importers_of(&barrels, scope, &names_one, true)?;
        barrels = Vec::new();
        for importer in found {
            let canonical = importer
                .path
                .canonicalize()
                .unwrap_or_else(|_| importer.path.clone());
            if !seen.insert(canonical.clone()) {
                continue;
            }
            if importer.reexports {
                barrels.push(canonical);
            }
            out.push(importer);
        }
    }
    Ok(out)
}

/// The files under `scope` whose imports resolve to one of `targets`
/// (canonical paths) by a statement whose text `keep` accepts, sorted by
/// path. A file is parsed only when its text names a target's module and
/// passes `keep`, so the walk parses few files. With `through`, the
/// targets are re-exporting files, and each importer records the one it
/// imports as its `via`.
fn importers_of(
    targets: &[PathBuf],
    scope: &Path,
    keep: &(dyn Fn(&str) -> bool + Sync),
    through: bool,
) -> Result<Vec<Importer>, TilthError> {
    let stems: Vec<String> = targets.iter().flat_map(|t| module_stems(t)).collect();
    let names: HashSet<&std::ffi::OsStr> = targets.iter().filter_map(|t| t.file_name()).collect();
    let found: Mutex<Vec<Importer>> = Mutex::new(Vec::new());
    let walker = super::walk::walker(scope, None)?;
    walker.run(|| {
        let found = &found;
        let stems = &stems;
        let names = &names;
        Box::new(move |entry| {
            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }
            let file = entry.path();
            if let Some(importer) = imports_one_of(file, targets, stems, names, keep, through) {
                found
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(importer);
            }
            ignore::WalkState::Continue
        })
    });
    let mut found = found
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    found.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(found)
}

/// `file` as an `Importer` of one of `targets`, if its imports name one.
fn imports_one_of(
    file: &Path,
    targets: &[PathBuf],
    stems: &[String],
    names: &HashSet<&std::ffi::OsStr>,
    keep: &(dyn Fn(&str) -> bool + Sync),
    through: bool,
) -> Option<Importer> {
    if std::fs::metadata(file).ok()?.len() > crate::source::MAX_FILE_SIZE {
        return None;
    }
    if !matches!(
        crate::lang::detect_file_type(file),
        FileType::Code(
            Lang::Rust
                | Lang::TypeScript
                | Lang::Tsx
                | Lang::JavaScript
                | Lang::Python
                | Lang::C
                | Lang::Cpp
                | Lang::Bash
        )
    ) {
        return None;
    }
    let content = crate::source::read_source(file).ok()?;
    if !stems
        .iter()
        .any(|s| memchr::memmem::find(content.as_bytes(), s.as_bytes()).is_some())
        || !keep(&content)
    {
        return None;
    }
    let FileType::Code(lang) = detect_file_type_with_content(file, &content) else {
        return None;
    };
    let mut lines = Vec::new();
    let mut statements = String::new();
    let mut via = None;
    let mut reexports = is_reexporting_file(file);
    let own_path = file.canonicalize().ok()?;
    for import in extract_imports(&content, lang) {
        let Some(resolved) = resolve_import(file, lang, &import) else {
            continue;
        };
        if !resolved.file_name().is_some_and(|n| names.contains(n)) {
            continue;
        }
        let resolved = resolved.canonicalize().unwrap_or(resolved);
        if !targets.contains(&resolved) || resolved == own_path {
            continue;
        }
        let statement = content
            .lines()
            .skip(import.start_line as usize - 1)
            .take((import.end_line - import.start_line) as usize + 1)
            .collect::<Vec<_>>()
            .join("\n");
        if !keep(&statement) {
            continue;
        }
        lines.push(import.start_line);
        if through {
            via.get_or_insert(resolved);
        }
        // `export { x } from "./target"` passes the target on.
        if matches!(lang, Lang::TypeScript | Lang::Tsx | Lang::JavaScript)
            && statement.trim_start().starts_with("export")
        {
            reexports = true;
        }
        statements.push_str(&statement);
        statements.push('\n');
    }
    if lines.is_empty() {
        return None;
    }
    lines.sort_unstable();
    lines.dedup();
    Some(Importer {
        path: file.to_path_buf(),
        lines,
        statements,
        via,
        reexports,
    })
}

/// A file whose role is to re-export its package: `index.ts`, `__init__.py`.
fn is_reexporting_file(file: &Path) -> bool {
    let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name == "__init__.py"
        || matches!(
            name.rsplit_once('.'),
            Some(("index", "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs"))
        )
}

/// The words an import of `path` contains one of: its file stem, and for
/// a package's entry file (`index.ts`, `__init__.py`, `mod.rs`) its
/// directory's name, which is how most imports name a package.
fn module_stems(path: &Path) -> Vec<String> {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return Vec::new();
    };
    let mut stems = vec![stem.to_string()];
    if matches!(stem, "index" | "__init__" | "mod") {
        if let Some(dir) = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
        {
            stems.push(dir.to_string());
        }
    }
    stems
}

/// `content` holds `name` as a whole identifier.
fn mentions(content: &str, name: &str) -> bool {
    let bytes = content.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    memchr::memmem::find_iter(bytes, name.as_bytes()).any(|at| {
        let before = at.checked_sub(1).map(|i| bytes[i]);
        let after = bytes.get(at + name.len()).copied();
        !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
    })
}

/// A name a call can spell: letters, digits, `_` and `$`.
fn is_identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$')
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

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn dependents_of(root: &Path, rel: &str) -> Vec<(String, Vec<String>, Option<String>)> {
        let bloom = crate::index::bloom::BloomFilterCache::new();
        let result = analyze_deps(&root.join(rel), root, &bloom).unwrap();
        result
            .used_by
            .into_iter()
            .map(|d| {
                let rel = |p: &Path| p.strip_prefix(root).unwrap().display().to_string();
                (
                    rel(&d.path),
                    d.symbols.into_iter().map(|(_, s, _)| s).collect(),
                    d.via.as_deref().map(rel),
                )
            })
            .collect()
    }

    /// A name exported under an alias is used under the alias. Searching
    /// only the local name, `plan.ts` had no dependents (#272).
    #[test]
    fn an_export_used_under_its_alias_has_dependents() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write(
            &root,
            "plan.ts",
            "function readPlan() { return 1; }\nexport { readPlan as parsePlan };\n",
        );
        write(
            &root,
            "use.ts",
            "import { parsePlan } from \"./plan\";\nparsePlan();\n",
        );
        let found = dependents_of(&root, "plan.ts");
        assert_eq!(
            found,
            [("use.ts".to_string(), vec!["parsePlan".to_string()], None)]
        );
    }

    /// A re-export is a use of the module it names (#271): a barrel that
    /// only re-exports used nothing.
    #[test]
    fn a_re_export_is_a_local_use() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write(&root, "y.ts", "export const y = 1;\n");
        write(&root, "z.ts", "export const z = 1;\n");
        write(
            &root,
            "index.ts",
            "export { y } from \"./y\";\nimport { z } from \"./z\";\n",
        );
        let bloom = crate::index::bloom::BloomFilterCache::new();
        let result = analyze_deps(&root.join("index.ts"), &root, &bloom).unwrap();
        let local: Vec<&Path> = result.uses_local.iter().map(|d| d.path.as_path()).collect();
        assert_eq!(local, [root.join("y.ts"), root.join("z.ts")]);
    }

    /// A Python file that imports a module and never calls into it still
    /// depends on it, and so does a file that imports it through its
    /// package's `__init__.py`. Found only through calls, the src-layout
    /// package below listed one of its four dependents.
    #[test]
    fn python_import_only_dependents_are_found() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write(&root, "pyproject.toml", "[project]\nname = \"pkg\"\n");
        write(&root, "src/pkg/__init__.py", "");
        write(&root, "src/pkg/sub/mod.py", "class Thing:\n    pass\n");
        write(&root, "src/pkg/sub/__init__.py", "from .mod import Thing\n");
        write(
            &root,
            "src/pkg/direct.py",
            "from pkg.sub.mod import Thing\n\nx: Thing = None\n",
        );
        write(
            &root,
            "src/pkg/caller.py",
            "from pkg.sub.mod import Thing\n\ndef make():\n    return Thing()\n",
        );
        write(
            &root,
            "tests/test_barrel.py",
            "from pkg.sub import Thing\n\ndef test_thing():\n    assert Thing\n",
        );
        // Imports the package for something else: no dependent.
        write(
            &root,
            "tests/test_other.py",
            "from pkg.sub import Other\n\ndef test_other():\n    assert Other\n",
        );
        let found = dependents_of(&root, "src/pkg/sub/mod.py");
        assert_eq!(
            found,
            [
                ("src/pkg/sub/__init__.py".to_string(), vec![], None),
                (
                    "src/pkg/caller.py".to_string(),
                    vec!["Thing".to_string()],
                    None
                ),
                ("src/pkg/direct.py".to_string(), vec![], None),
                (
                    "tests/test_barrel.py".to_string(),
                    vec![],
                    Some("src/pkg/sub/__init__.py".to_string())
                ),
            ]
        );
    }

    /// A TS barrel passes its module on: a file that imports a name from
    /// the barrel depends on the module that defines it.
    #[test]
    fn ts_importers_through_a_barrel_are_dependents() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write(
            &root,
            "src/service.ts",
            "export function serve() { return 1; }\n",
        );
        write(&root, "src/index.ts", "export * from \"./service\";\n");
        write(
            &root,
            "src/barrel2.ts",
            "export { serve } from \"./service\";\n",
        );
        write(
            &root,
            "c.ts",
            "import { serve } from \"./src\";\nexport const s = serve;\n",
        );
        write(
            &root,
            "d.ts",
            "import { serve } from \"./src/barrel2\";\nconst handler = serve;\n",
        );
        write(
            &root,
            "e.ts",
            "import { other } from \"./src\";\nother();\n",
        );
        let found = dependents_of(&root, "src/service.ts");
        assert_eq!(
            found,
            [
                ("src/barrel2.ts".to_string(), vec![], None),
                ("src/index.ts".to_string(), vec![], None),
                ("c.ts".to_string(), vec![], Some("src/index.ts".to_string())),
                (
                    "d.ts".to_string(),
                    vec![],
                    Some("src/barrel2.ts".to_string())
                ),
            ]
        );
    }

    /// Past the cap, the names other files import are searched first, and
    /// the rest are reported. By length alone, the short and central `run`
    /// was the one left out (#285).
    #[test]
    fn past_the_cap_the_names_importers_use_are_searched() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let mut target = String::from("def run():\n    pass\n");
        for i in 0..MAX_EXPORTED_SYMBOLS + 4 {
            let _ = write!(
                target,
                "\ndef a_rather_long_helper_name_{i:02}():\n    pass\n"
            );
        }
        write(&root, "lib.py", &target);
        write(
            &root,
            "app.py",
            "from lib import run\n\ndef main():\n    run()\n",
        );
        let bloom = crate::index::bloom::BloomFilterCache::new();
        let result = analyze_deps(&root.join("lib.py"), &root, &bloom).unwrap();
        assert_eq!(result.exported_count, MAX_EXPORTED_SYMBOLS + 5);
        assert_eq!(result.unsearched.len(), 5, "{:?}", result.unsearched);
        assert!(!result.unsearched.contains(&"run".to_string()));
        let app = result
            .used_by
            .iter()
            .find(|d| d.path.ends_with("app.py"))
            .unwrap();
        assert_eq!(
            app.symbols.iter().map(|s| s.1.as_str()).collect::<Vec<_>>(),
            ["run"]
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
