//! File-level dependency analysis: what a file imports and what imports it.
//! Used by `tilth_deps` for blast-radius checks before breaking changes.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::cache::OutlineCache;
use crate::error::TilthError;
use crate::lang::detect_file_type;
use crate::lang::outline::{extract_import_source, get_outline_entries};
use crate::read::imports::{is_external, is_import_line, resolve_related_files_with_content};
use crate::search::callees::{extract_callee_names, resolve_callees};
use crate::search::callers::find_callers_batch;
use crate::types::{FileType, OutlineKind};

/// Maximum number of exported symbols to search for in the reverse direction.
const MAX_EXPORTED_SYMBOLS: usize = 25;

/// Maximum number of dependents to show before truncation.
const MAX_DEPENDENTS: usize = 15;

/// Result of a full dependency analysis for a single file.
pub struct DepsResult {
    pub target: PathBuf,
    pub uses_local: Vec<LocalDep>,
    pub uses_external: Vec<String>,
    pub used_by: Vec<Dependent>,
    /// Total dependents found before truncation.
    pub total_dependents: usize,
    pub exported_count: usize,
    /// Actual number of symbols searched (may be < `exported_count` if capped).
    pub searched_count: usize,
}

/// A local file dependency with the symbols used from it.
pub struct LocalDep {
    pub path: PathBuf,
    pub symbols: Vec<String>,
}

/// A file that depends on the target, with symbol-level call detail.
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
    cache: &OutlineCache,
    bloom: &crate::index::bloom::BloomFilterCache,
) -> Result<DepsResult, TilthError> {
    // Canonicalize for reliable path comparison (callers return absolute paths).
    let path = &path.canonicalize().map_err(|e| TilthError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;

    let content = fs::read_to_string(path).map_err(|e| TilthError::IoError {
        path: path.clone(),
        source: e,
    })?;

    let FileType::Code(lang) = detect_file_type(path) else {
        // Non-code file: return empty deps gracefully.
        return Ok(DepsResult {
            target: path.clone(),
            uses_local: Vec::new(),
            uses_external: Vec::new(),
            used_by: Vec::new(),
            total_dependents: 0,
            exported_count: 0,
            searched_count: 0,
        });
    };

    // ── Phase 1: Extract exported symbols ────────────────────────────────────

    let entries = get_outline_entries(&content, lang);
    let _ = cache; // available for future caching

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
    let resolved = resolve_callees(&callee_names, path, &content, cache, bloom);

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

    // External deps via line-level import parsing
    let mut external_set: HashSet<String> = HashSet::new();
    for line in content.lines() {
        if !is_import_line(line, lang) {
            continue;
        }
        let source = extract_import_source(line);
        if source.is_empty() {
            continue;
        }
        if is_external(&source, lang) && !is_stdlib(&source, lang) && is_valid_module_path(&source)
        {
            external_set.insert(source.clone());
        }
    }
    let mut uses_external: Vec<String> = external_set.into_iter().collect();
    uses_external.sort();

    // ── Phase 3: Reverse dependencies ────────────────────────────────────────

    let mut used_by = if searched_count > 0 {
        let symbols_set: HashSet<String> = all_names.iter().cloned().collect();
        let raw_matches = find_callers_batch(&symbols_set, scope, bloom, None)?;

        // Group by file path
        let mut by_file: HashMap<PathBuf, Vec<(String, String, u32)>> = HashMap::new();
        for (matched_symbol, caller_match) in raw_matches {
            // Exclude calls from within the target file itself (self-references)
            if caller_match.path == *path {
                continue;
            }
            by_file.entry(caller_match.path).or_default().push((
                caller_match.calling_function,
                matched_symbol,
                caller_match.line,
            ));
        }

        // Build Dependent list
        let target_dir = path.parent();
        let mut dependents: Vec<Dependent> = by_file
            .into_iter()
            .map(|(dep_path, mut pairs)| {
                pairs.sort();
                pairs.dedup();
                let is_test = is_test_file(&dep_path);
                Dependent {
                    path: dep_path,
                    symbols: pairs,
                    is_test,
                }
            })
            .collect();

        // Sort: same directory first, non-tests before tests, then alphabetical
        dependents.sort_by(|a, b| {
            let a_same_dir = target_dir.is_some_and(|d| a.path.parent() == Some(d));
            let b_same_dir = target_dir.is_some_and(|d| b.path.parent() == Some(d));
            b_same_dir
                .cmp(&a_same_dir)
                .then_with(|| a.is_test.cmp(&b.is_test))
                .then_with(|| a.path.cmp(&b.path))
        });

        dependents
    } else {
        Vec::new()
    };

    let total_dependents = used_by.len();
    used_by.truncate(MAX_DEPENDENTS);

    Ok(DepsResult {
        target: path.clone(),
        uses_local,
        uses_external,
        used_by,
        total_dependents,
        exported_count,
        searched_count,
    })
}

/// Format a `DepsResult` as a compact, readable string.
///
/// Budget truncation priority (when `budget` tokens is too tight):
/// 1. Truncate "Used by" entries (keep header count)
/// 2. Truncate "Uses (external)" to count only
/// 3. Truncate "Uses (local)" symbol lists to file paths only
/// 4. Never truncate the header line
pub fn format_deps(result: &DepsResult, scope: &Path, budget: Option<usize>) -> String {
    let dep_count = result.total_dependents;
    let (prod_deps, test_deps): (Vec<_>, Vec<_>) = result.used_by.iter().partition(|d| !d.is_test);

    // ── Build sections (full fidelity first) ─────────────────────────────────

    // Header
    let rel_target = result
        .target
        .strip_prefix(scope)
        .unwrap_or(&result.target)
        .display()
        .to_string();
    let header = format!(
        "# Deps: {} — {} local, {} external, {} dependent{}",
        rel_target,
        result.uses_local.len(),
        result.uses_external.len(),
        dep_count,
        if dep_count == 1 { "" } else { "s" },
    );

    let uses_local_section = format_uses_local(&result.uses_local, scope, true);
    let uses_external_section = format_uses_external(&result.uses_external);
    let used_by_section = format_used_by(&prod_deps, scope, "## Used by");
    let used_by_tests_section = format_used_by(&test_deps, scope, "## Used by (tests)");

    let barrel_note = if result.exported_count > MAX_EXPORTED_SYMBOLS {
        format!(
            "\n\n> ({} of {} exports shown — barrel file detected)",
            result.searched_count, result.exported_count
        )
    } else {
        String::new()
    };

    // Full output
    let mut parts: Vec<String> = Vec::new();
    parts.push(header.clone());
    if !uses_local_section.is_empty() {
        parts.push(uses_local_section.clone());
    }
    if !uses_external_section.is_empty() {
        parts.push(uses_external_section.clone());
    }
    if !used_by_section.is_empty() {
        parts.push(used_by_section.clone());
    }
    if !used_by_tests_section.is_empty() {
        parts.push(used_by_tests_section.clone());
    }
    let truncated = result.total_dependents.saturating_sub(result.used_by.len());
    if truncated > 0 {
        parts.push(format!("... and {truncated} more dependents"));
    }
    if !barrel_note.is_empty() {
        parts.push(barrel_note.clone());
    }

    let full = parts.join("\n\n");
    let full_tokens = crate::types::estimate_tokens(full.len() as u64) as usize;

    let output = match budget {
        None => full,
        Some(b) if full_tokens <= b => full,
        Some(b) => {
            // Apply truncation in priority order
            apply_budget_truncation(
                &header,
                &uses_local_section,
                &uses_external_section,
                &prod_deps,
                &test_deps,
                &barrel_note,
                scope,
                b,
            )
        }
    };

    let token_est = crate::types::estimate_tokens(output.len() as u64);
    format!("{output}\n\n[~{token_est} tokens]")
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
    use crate::types::Lang;
    match lang {
        Lang::Rust => {
            source.starts_with("std::")
                || source.starts_with("core::")
                || source.starts_with("alloc::")
        }
        Lang::Python => {
            // Common stdlib modules — not exhaustive but covers the noisy ones
            matches!(
                source.split('.').next().unwrap_or(""),
                "os" | "sys"
                    | "re"
                    | "json"
                    | "math"
                    | "time"
                    | "datetime"
                    | "pathlib"
                    | "typing"
                    | "collections"
                    | "functools"
                    | "itertools"
                    | "abc"
                    | "io"
                    | "logging"
                    | "unittest"
                    | "dataclasses"
                    | "enum"
                    | "copy"
                    | "hashlib"
                    | "subprocess"
                    | "threading"
                    | "asyncio"
            )
        }
        Lang::Go => source.starts_with("fmt") || !source.contains('.'),
        _ => false,
    }
}

/// Returns true if the string looks like a valid module/package path.
/// Filters out garbage from string literals that pass `is_import_line`.
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

use crate::types::is_test_file;

/// Format the "Uses (local)" section.
fn format_uses_local(deps: &[LocalDep], scope: &Path, with_symbols: bool) -> String {
    if deps.is_empty() {
        return String::new();
    }
    let mut out = String::from("## Uses (local)");
    for dep in deps {
        let rel = dep
            .path
            .strip_prefix(scope)
            .unwrap_or(&dep.path)
            .display()
            .to_string();
        if with_symbols && !dep.symbols.is_empty() {
            let _ = write!(out, "\n{:<30} {}", rel, dep.symbols.join(", "));
        } else {
            let _ = write!(out, "\n{rel}");
        }
    }
    out
}

/// Format the "Uses (external)" section.
fn format_uses_external(externals: &[String]) -> String {
    if externals.is_empty() {
        return String::new();
    }
    let mut out = String::from("## Uses (external)");
    for ext in externals {
        let _ = write!(out, "\n{ext}");
    }
    out
}

/// Format a "Used by" section from a slice of dependents.
fn format_used_by(deps: &[&Dependent], scope: &Path, heading: &str) -> String {
    if deps.is_empty() {
        return String::new();
    }
    let mut out = String::from(heading);
    for dep in deps {
        let rel = dep
            .path
            .strip_prefix(scope)
            .unwrap_or(&dep.path)
            .display()
            .to_string();
        // Group by (caller, line) for readability — keep the earliest line per caller
        let mut by_caller: HashMap<&str, (u32, Vec<&str>)> = HashMap::new();
        for (caller, symbol, line) in &dep.symbols {
            let entry = by_caller
                .entry(caller.as_str())
                .or_insert((*line, Vec::new()));
            entry.0 = entry.0.min(*line);
            entry.1.push(symbol.as_str());
        }
        let mut callers: Vec<(&str, u32, Vec<&str>)> = by_caller
            .into_iter()
            .map(|(caller, (line, syms))| (caller, line, syms))
            .collect();
        callers.sort_unstable_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));
        for (caller, line, syms) in callers {
            let loc = format!("{rel}:{line}");
            let joined = syms.join(", ");
            let _ = write!(out, "\n{loc:<30} {caller:<20} \u{2192} {joined}");
        }
    }
    out
}

/// Apply progressive budget truncation and reassemble the output.
#[allow(clippy::too_many_arguments)]
fn apply_budget_truncation(
    header: &str,
    uses_local_full: &str,
    uses_external_full: &str,
    prod_deps: &[&Dependent],
    test_deps: &[&Dependent],
    barrel_note: &str,
    scope: &Path,
    budget: usize,
) -> String {
    // Try progressively degraded versions
    #[allow(clippy::type_complexity)]
    let candidates: &[fn(
        &str,
        &str,
        &str,
        &[&Dependent],
        &[&Dependent],
        &str,
        &Path,
    ) -> String] = &[
        // Level 0: no tests
        |hdr, ul, ue, pd, _td, bn, sc| {
            assemble(&[hdr, ul, ue, &format_used_by(pd, sc, "## Used by"), bn])
        },
        // Level 1: no used-by entries at all
        |hdr, ul, ue, pd, _td, bn, _sc| {
            let count = pd.len();
            let note = if count > 0 {
                format!("\n\n(... {count} more dependents)")
            } else {
                String::new()
            };
            assemble(&[hdr, ul, ue, &note, bn])
        },
        // Level 2: external as count only
        |hdr, ul, _ue, _pd, _td, bn, _sc| assemble(&[hdr, ul, bn]),
        // Level 3: local as paths only (no symbols)
        |hdr, ul, _ue, _pd, _td, _bn, _sc| {
            // Strip symbol lists: each line is "path_padded  symbols" — take only up to first space run
            let local_lines: Vec<&str> = ul
                .lines()
                .skip(1) // skip heading
                .map(|l| l.split_whitespace().next().unwrap_or(l))
                .collect();
            let paths_only = if local_lines.is_empty() {
                String::new()
            } else {
                format!("## Uses (local)\n{}", local_lines.join("\n"))
            };
            assemble(&[hdr, &paths_only])
        },
        // Level 4: header only
        |hdr, _ul, _ue, _pd, _td, _bn, _sc| hdr.to_string(),
    ];

    for candidate_fn in candidates {
        let candidate = candidate_fn(
            header,
            uses_local_full,
            uses_external_full,
            prod_deps,
            test_deps,
            barrel_note,
            scope,
        );
        let tokens = crate::types::estimate_tokens(candidate.len() as u64) as usize;
        if tokens <= budget {
            return candidate;
        }
    }

    // Absolute fallback: just the header
    header.to_string()
}

/// Join non-empty parts with double newlines.
fn assemble(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|s| !s.trim().is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n\n")
}
