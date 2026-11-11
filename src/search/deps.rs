//! `tilth_deps` output: renders the `DepsResult` that `tilth_core` computes,
//! with budget truncation.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use tilth_core::search::deps::{Dependent, DepsResult, LocalDep, MAX_EXPORTED_SYMBOLS};

/// Dependents listed before the rest are summarised in a count, unless the
/// caller asks for the full list.
const MAX_DEPENDENTS: usize = 15;

/// Format a `DepsResult` as a compact, readable string.
///
/// `full` lists every dependent; otherwise the first `MAX_DEPENDENTS` are
/// listed and the rest counted.
///
/// Budget truncation priority (when `budget` tokens is too tight):
/// 1. Truncate "Used by" entries (keep header count)
/// 2. Truncate "Uses (external)" to count only
/// 3. Truncate "Uses (local)" symbol lists to file paths only
/// 4. Never truncate the header line
///
/// Whenever dependents are left out, a line says how many.
pub fn format_deps(result: &DepsResult, scope: &Path, budget: Option<usize>, full: bool) -> String {
    let dep_count = result.used_by.len();
    let listed = if full {
        dep_count
    } else {
        dep_count.min(MAX_DEPENDENTS)
    };
    let (prod_deps, test_deps): (Vec<_>, Vec<_>) =
        result.used_by[..listed].iter().partition(|d| !d.is_test);

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

    let unsearched = unsearched_note(result);

    // Full output
    let full_output = assemble(&[
        &header,
        &uses_local_section,
        &uses_external_section,
        &used_by_section,
        &used_by_tests_section,
        &unlisted_dependents(dep_count, listed),
        &unsearched,
    ]);
    let full_tokens = crate::types::estimate_tokens(full_output.len() as u64) as usize;

    let output = match budget {
        None => full_output,
        Some(b) if full_tokens <= b => full_output,
        Some(b) => {
            // Apply truncation in priority order
            apply_budget_truncation(
                &header,
                &uses_local_section,
                &uses_external_section,
                &prod_deps,
                dep_count,
                &unsearched,
                scope,
                b,
            )
        }
    };

    // Kept under any budget: a skipped file may hold dependents nobody checked.
    let output = output + &crate::search::skipped_files_note(&result.skipped_large, scope, &[]);

    let token_est = crate::types::estimate_tokens(output.len() as u64);
    format!("{output}\n\n[~{token_est} tokens]")
}

/// The note that names what the call-site search left out, or nothing when
/// it searched every name. Importers are found whatever the cap.
fn unsearched_note(result: &DepsResult) -> String {
    /// Left-out names spelled out before the rest are counted.
    const NAMED: usize = 10;
    let left_out = &result.unsearched;
    if left_out.is_empty() {
        return String::new();
    }
    let mut named = left_out
        .iter()
        .take(NAMED)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if left_out.len() > NAMED {
        let _ = write!(named, " and {} more", left_out.len() - NAMED);
    }
    format!(
        "> Callers searched for {MAX_EXPORTED_SYMBOLS} of {} names: first the ones other \
         files import by name, then the longest. Not searched: {named}.",
        result.exported_count
    )
}

/// The line that counts dependents left out of the listing, or nothing when
/// all `total` are listed.
fn unlisted_dependents(total: usize, listed: usize) -> String {
    match total.saturating_sub(listed) {
        0 => String::new(),
        n if listed == 0 => format!(
            "... {n} dependent{} not listed",
            if n == 1 { "" } else { "s" }
        ),
        n => format!(
            "... and {n} more dependent{}",
            if n == 1 { "" } else { "s" }
        ),
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

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
        for (caller, line, syms) in &callers {
            let loc = format!("{rel}:{line}");
            let joined = syms.join(", ");
            let _ = write!(out, "\n{loc:<30} {caller:<20} \u{2192} {joined}");
        }
        // A file that imports the target without calling it.
        if callers.is_empty() {
            if let Some(line) = dep.import_lines.first() {
                let loc = format!("{rel}:{line}");
                match &dep.via {
                    Some(via) => {
                        let via = via.strip_prefix(scope).unwrap_or(via).display();
                        let _ = write!(out, "\n{loc:<30} imports it through {via}");
                    }
                    None => {
                        let _ = write!(out, "\n{loc:<30} imports it");
                    }
                }
            }
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
    total_dependents: usize,
    unsearched: &str,
    scope: &Path,
    budget: usize,
) -> String {
    // Local deps as paths only, without their symbol lists. Each line is
    // "path_padded  symbols": keep the path.
    let local_paths_only = {
        let local_lines: Vec<&str> = uses_local_full
            .lines()
            .skip(1) // skip heading
            .map(|l| l.split_whitespace().next().unwrap_or(l))
            .collect();
        if local_lines.is_empty() {
            String::new()
        } else {
            format!("## Uses (local)\n{}", local_lines.join("\n"))
        }
    };
    let prod_listed = format_used_by(prod_deps, scope, "## Used by");
    let prod_unlisted = unlisted_dependents(total_dependents, prod_deps.len());
    let none_listed = unlisted_dependents(total_dependents, 0);

    // Progressively degraded versions, richest first.
    let candidates: [Vec<&str>; 5] = [
        // Level 0: no tests
        vec![
            header,
            uses_local_full,
            uses_external_full,
            &prod_listed,
            &prod_unlisted,
            unsearched,
        ],
        // Level 1: no used-by entries at all
        vec![
            header,
            uses_local_full,
            uses_external_full,
            &none_listed,
            unsearched,
        ],
        // Level 2: external as count only
        vec![header, uses_local_full, &none_listed, unsearched],
        // Level 3: local as paths only (no symbols)
        vec![header, &local_paths_only, &none_listed],
        // Level 4: header only
        vec![header],
    ];

    for parts in &candidates {
        let candidate = assemble(parts);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 20 dependents, the last 5 of them tests, as `analyze_deps` orders them.
    fn twenty_dependents() -> DepsResult {
        let used_by = (0..20)
            .map(|i| Dependent {
                path: PathBuf::from(format!("/r/user{i:02}.ts")),
                symbols: vec![(format!("user{i}"), "libFn".to_string(), 2)],
                is_test: i >= 15,
                import_lines: vec![1],
                via: None,
            })
            .collect();
        DepsResult {
            target: PathBuf::from("/r/lib.ts"),
            uses_local: Vec::new(),
            uses_external: Vec::new(),
            used_by,
            skipped_large: Vec::new(),
            exported_count: 1,
            unsearched: Vec::new(),
        }
    }

    /// A dependent that imports the target without calling it had no line,
    /// and the old note claimed a barrel file for any file with many names.
    #[test]
    fn importers_and_unsearched_names_are_shown() {
        let dependent = |name: &str, via: Option<&str>| Dependent {
            path: PathBuf::from(format!("/r/{name}")),
            symbols: Vec::new(),
            is_test: false,
            import_lines: vec![3],
            via: via.map(|v| PathBuf::from(format!("/r/{v}"))),
        };
        let result = DepsResult {
            target: PathBuf::from("/r/lib.py"),
            uses_local: Vec::new(),
            uses_external: Vec::new(),
            used_by: vec![
                dependent("a.py", None),
                dependent("b.py", Some("pkg/__init__.py")),
            ],
            skipped_large: Vec::new(),
            exported_count: 27,
            unsearched: vec!["get_dependant".to_string(), "x_y".to_string()],
        };
        let out = format_deps(&result, Path::new("/r"), None, false);
        assert!(
            out.contains("\na.py:3                         imports it\n"),
            "{out}"
        );
        assert!(
            out.contains("\nb.py:3                         imports it through pkg/__init__.py"),
            "{out}"
        );
        assert!(
            out.contains("> Callers searched for 25 of 27 names: first the ones other files import by name, then the longest. Not searched: get_dependant, x_y."),
            "{out}"
        );
        assert!(!out.contains("barrel file detected"), "{out}");
    }

    fn listed(out: &str) -> usize {
        out.lines().filter(|l| l.starts_with("user")).count()
    }

    #[test]
    fn default_lists_fifteen_and_counts_the_rest() {
        let out = format_deps(&twenty_dependents(), Path::new("/r"), None, false);
        assert!(out.contains("20 dependents"), "{out}");
        assert_eq!(listed(&out), 15, "{out}");
        assert!(out.contains("... and 5 more dependents"), "{out}");
    }

    /// `--full` used to change nothing: the cap sat in the analysis.
    #[test]
    fn full_lists_every_dependent() {
        let out = format_deps(&twenty_dependents(), Path::new("/r"), None, true);
        assert_eq!(listed(&out), 20, "{out}");
        assert!(out.contains("## Used by (tests)"), "{out}");
        assert!(!out.contains("more dependents"), "{out}");
    }

    /// Under a budget the tests section went first with nothing said, and
    /// the next level counted the dependents it showed as the ones it hid.
    #[test]
    fn a_budget_cut_says_how_many_dependents_are_not_listed() {
        let result = twenty_dependents();
        let full = format_deps(&result, Path::new("/r"), None, true);
        let full_tokens = crate::types::estimate_tokens(full.len() as u64) as usize;

        let no_tests = format_deps(&result, Path::new("/r"), Some(full_tokens - 20), true);
        assert_eq!(listed(&no_tests), 15, "{no_tests}");
        assert!(no_tests.contains("... and 5 more dependents"), "{no_tests}");

        let none = format_deps(&result, Path::new("/r"), Some(30), true);
        assert_eq!(listed(&none), 0, "{none}");
        assert!(none.contains("... 20 dependents not listed"), "{none}");
    }
}
