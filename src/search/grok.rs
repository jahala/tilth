//! Grok orchestrator: collapse the "search → expand → search-callers → read-context"
//! dance into one structured response. A1 ships target resolution; A2 assembles
//! callees/callers/siblings/tests; A3 will format.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::TilthError;
use crate::index::bloom::BloomFilterCache;
use crate::lang::detect_file_type;
use crate::lang::outline::get_outline_entries;
use crate::search::callees::{extract_callee_names, resolve_callees, ResolvedCallee};
use crate::search::callers::{find_callers_batch, CallerMatch, BATCH_EARLY_QUIT};
use crate::search::search_symbol_raw;
use crate::types::{is_test_file, FileType, Lang, OutlineEntry, OutlineKind};

/// What grok resolved the user's target string to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    pub name: String,
    pub path: PathBuf,
    pub start_line: u32,
    pub end_line: u32,
    pub kind: OutlineKind,
    pub signature: Option<String>,
    pub doc: Option<String>,
    /// Count of *other* matching definitions when resolving by name.
    /// 0 means unambiguous; >0 means the formatter should warn the agent.
    pub other_def_count: usize,
}

/// Parsed form of the target spec passed to grok.
#[derive(Debug, PartialEq, Eq)]
enum TargetSpec {
    Symbol(String),
    PathLine { path: PathBuf, line: u32 },
}

/// Parse a target spec string. `foo` → `Symbol`, `src/foo.rs:7` → `PathLine`.
///
/// Disambiguates `path:line` from `Box::new` by requiring:
///   1. The right side after the last `:` is purely digits, and
///   2. The left side contains `/` or `.` (path indicators).
///
/// `Type::method` survives because `::method` won't parse as a digit.
fn parse_target_spec(s: &str) -> TargetSpec {
    if let Some(idx) = s.rfind(':') {
        let (left, right) = s.split_at(idx);
        let right = &right[1..];
        if !right.is_empty()
            && right.bytes().all(|b| b.is_ascii_digit())
            && (left.contains('/') || left.contains('.'))
        {
            if let Ok(line) = right.parse::<u32>() {
                return TargetSpec::PathLine {
                    path: PathBuf::from(left),
                    line,
                };
            }
        }
    }
    TargetSpec::Symbol(s.to_string())
}

/// Resolve a target spec and return the loaded source plus its detected
/// language. Single file read; single outline parse downstream.
fn resolve_with_source(
    spec: &str,
    scope: &Path,
) -> Result<(ResolvedTarget, String, Lang), TilthError> {
    match parse_target_spec(spec) {
        TargetSpec::Symbol(name) => resolve_by_name(&name, scope),
        TargetSpec::PathLine { path, line } => {
            let path = if path.is_absolute() {
                path
            } else {
                scope.join(path)
            };
            resolve_by_path_line(&path, line)
        }
    }
}

fn resolve_by_name(name: &str, scope: &Path) -> Result<(ResolvedTarget, String, Lang), TilthError> {
    let result = search_symbol_raw(name, scope, None)?;
    let definitions: Vec<_> = result.matches.iter().filter(|m| m.is_definition).collect();
    let Some(top) = definitions.first() else {
        return Err(TilthError::NotFound {
            path: PathBuf::from(name),
            suggestion: None,
        });
    };
    let other_def_count = definitions.len().saturating_sub(1);
    let (start, _end) = top.def_range.ok_or_else(|| TilthError::ParseError {
        path: top.path.clone(),
        reason: format!("definition match for `{name}` had no def_range"),
    })?;
    enrich_from_outline(top.path.clone(), start, name.to_string(), other_def_count)
}

fn resolve_by_path_line(
    path: &Path,
    line: u32,
) -> Result<(ResolvedTarget, String, Lang), TilthError> {
    let (content, lang) = read_code_file(path)?;
    let entries = get_outline_entries(&content, lang);
    let entry = find_entry_at_line(&entries, line).ok_or_else(|| TilthError::NotFound {
        path: path.to_path_buf(),
        suggestion: Some(format!("no definition encloses line {line}")),
    })?;
    let target = target_from_entry(entry, path.to_path_buf(), 0);
    Ok((target, content, lang))
}

/// Read `path` and detect its language. Errors if the file isn't a code file —
/// grok requires source-level analysis, not a markdown / config / data file.
fn read_code_file(path: &Path) -> Result<(String, Lang), TilthError> {
    let content = fs::read_to_string(path).map_err(|e| TilthError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;
    let FileType::Code(lang) = detect_file_type(path) else {
        return Err(TilthError::InvalidQuery {
            query: path.display().to_string(),
            reason: "not a code file — grok needs source code".to_string(),
        });
    };
    Ok((content, lang))
}

/// Read the file at `path`, find the outline entry that starts at `start_line`
/// (or the deepest entry enclosing it), and convert to `ResolvedTarget`.
fn enrich_from_outline(
    path: PathBuf,
    start_line: u32,
    name: String,
    other_def_count: usize,
) -> Result<(ResolvedTarget, String, Lang), TilthError> {
    let (content, lang) = read_code_file(&path)?;
    let entries = get_outline_entries(&content, lang);
    let entry = find_by_start_line(&entries, start_line)
        .or_else(|| find_entry_at_line(&entries, start_line));
    let target = match entry {
        Some(e) => target_from_entry(e, path, other_def_count),
        None => ResolvedTarget {
            name,
            path,
            start_line,
            end_line: start_line,
            kind: OutlineKind::Function,
            signature: None,
            doc: None,
            other_def_count,
        },
    };
    Ok((target, content, lang))
}

fn target_from_entry(
    entry: &OutlineEntry,
    path: PathBuf,
    other_def_count: usize,
) -> ResolvedTarget {
    ResolvedTarget {
        name: entry.name.clone(),
        path,
        start_line: entry.start_line,
        end_line: entry.end_line,
        kind: entry.kind,
        signature: entry.signature.clone(),
        doc: entry.doc.clone(),
        other_def_count,
    }
}

/// Walk the outline tree and return the deepest entry whose range contains `line`.
fn find_entry_at_line(entries: &[OutlineEntry], line: u32) -> Option<&OutlineEntry> {
    let mut best: Option<&OutlineEntry> = None;
    for e in entries {
        if line >= e.start_line && line <= e.end_line {
            if let Some(deeper) = find_entry_at_line(&e.children, line) {
                return Some(deeper);
            }
            best = Some(e);
        }
    }
    best
}

/// Walk the outline tree and return the first entry whose `start_line == line`.
fn find_by_start_line(entries: &[OutlineEntry], line: u32) -> Option<&OutlineEntry> {
    for e in entries {
        if e.start_line == line {
            return Some(e);
        }
        if let Some(child) = find_by_start_line(&e.children, line) {
            return Some(child);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// A2: bundle assembly
// ---------------------------------------------------------------------------

/// Per-section truncation caps. Defaults are strict — the point is to save context.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_field_names)]
pub struct GrokCaps {
    pub max_callees: usize,
    pub max_callers: usize,
    pub max_siblings: usize,
    pub max_tests: usize,
    /// Maximum lines of the target's own body to inline. Longer bodies are
    /// elided in the middle. Set to 0 to suppress the body entirely.
    pub max_body_lines: usize,
}

impl Default for GrokCaps {
    fn default() -> Self {
        Self {
            max_callees: 5,
            max_callers: 5,
            max_siblings: 8,
            max_tests: 8,
            max_body_lines: 60,
        }
    }
}

impl GrokCaps {
    /// `--full` caps — wider, but still bounded.
    #[must_use]
    pub fn full() -> Self {
        Self {
            max_callees: 30,
            max_callers: 50,
            max_siblings: 30,
            max_tests: 30,
            max_body_lines: 200,
        }
    }
}

/// A sibling definition (peer method on the same parent, or peer top-level def
/// in the same file). Signature only — never the body.
#[derive(Debug, Clone)]
pub struct SiblingEntry {
    pub name: String,
    pub kind: OutlineKind,
    pub start_line: u32,
    pub end_line: u32,
    pub signature: Option<String>,
}

/// A reference to the target from a test file. `test_name` comes from the
/// enclosing function the call appears in (already populated by the callers walk).
#[derive(Debug, Clone)]
pub struct TestMatch {
    pub path: PathBuf,
    pub line: u32,
    pub test_name: String,
}

/// The assembled grok bundle. `total_*` fields are pre-truncation counts so the
/// formatter can render "shown N of M" headings.
#[derive(Debug)]
pub struct GrokResult {
    pub target: ResolvedTarget,
    /// The target's own source body, sliced from the file. Empty when the
    /// body span is degenerate (`start_line > end_line`) or when the target
    /// resolution didn't surface a real outline entry. Stored as a string
    /// so the formatter can wrap it in a fenced block without re-reading.
    pub body: String,
    pub callees_internal: Vec<ResolvedCallee>,
    pub callees_external: Vec<String>,
    pub callers: Vec<CallerMatch>,
    pub siblings: Vec<SiblingEntry>,
    pub tests: Vec<TestMatch>,
    pub total_callees_internal: usize,
    pub total_callees_external: usize,
    pub total_callers: usize,
    pub total_siblings: usize,
    pub total_tests: usize,
}

/// Top-level entry: resolve the spec, then gather the structural neighborhood.
///
/// Performs a single read of the target file (via `resolve_with_source`), reuses
/// that content for both outline + callee extraction. The callers walk and the
/// callees walk happen against the live filesystem — they each carry their own
/// TOCTOU window, but stitching them into one bundle is bounded by the call
/// duration and acceptable for an interactive code-intelligence tool.
pub fn grok(
    target_spec: &str,
    scope: &Path,
    bloom: &BloomFilterCache,
    caps: GrokCaps,
) -> Result<GrokResult, TilthError> {
    let (target, content, lang) = resolve_with_source(target_spec, scope)?;
    let entries = get_outline_entries(&content, lang);
    let body = slice_body(
        &content,
        target.start_line,
        target.end_line,
        caps.max_body_lines,
    );

    // --- Callees -----------------------------------------------------------
    let callee_names =
        extract_callee_names(&content, lang, Some((target.start_line, target.end_line)));
    let resolved = resolve_callees(&callee_names, &target.path, &content, bloom);

    let resolved_names: HashSet<&str> = resolved.iter().map(|c| c.name.as_str()).collect();
    let externals: Vec<String> = callee_names
        .iter()
        .filter(|n| !resolved_names.contains(n.as_str()))
        .cloned()
        .collect();

    // The target's path may differ from canonical form (symlinks, `../` segments).
    // The walker hands us canonical absolute paths in match/callee results; we
    // canonicalize once so the self-reference filter below can compare cleanly.
    // Fallback to raw target.path is harmless — comparisons will simply never
    // match, and recursion will surface in the callers section (still correct,
    // just unfiltered).
    let canonical_target = target
        .path
        .canonicalize()
        .unwrap_or_else(|_| target.path.clone());

    // Drop self-references: a recursive call shows up as a "callee" because the
    // tree-sitter query matches every call expression, including those pointing
    // back at the target itself. Keep callers (where recursion is meaningful)
    // and filter callees (where recursion is just noise about the target's own
    // identifier appearing in its body).
    let resolved: Vec<ResolvedCallee> = resolved
        .into_iter()
        .filter(|c| !is_self_definition(c, &canonical_target, &target))
        .collect();

    let total_callees_internal = resolved.len();
    let total_callees_external = externals.len();

    // --- Callers + tests (one walk, partitioned by is_test_file) ----------
    let symbols: HashSet<String> = std::iter::once(target.name.clone()).collect();
    let raw_callers = find_callers_batch(&symbols, scope, bloom, None, BATCH_EARLY_QUIT)?;

    let prod_and_test: Vec<CallerMatch> = raw_callers
        .into_iter()
        .map(|(_, m)| m)
        .filter(|m| !is_recursive_call_site(m, &canonical_target, &target))
        .collect();

    let (prod_callers, test_callers): (Vec<_>, Vec<_>) = prod_and_test
        .into_iter()
        .partition(|m| !is_test_file(&m.path));
    let total_callers = prod_callers.len();
    let total_tests = test_callers.len();

    // --- Siblings ----------------------------------------------------------
    let siblings_all = collect_siblings(&entries, &target);
    let total_siblings = siblings_all.len();

    // --- Apply caps --------------------------------------------------------
    let mut callees_internal = resolved;
    callees_internal.truncate(caps.max_callees);

    let mut callees_external = externals;
    callees_external.truncate(caps.max_callees);

    let mut callers = prod_callers;
    callers.truncate(caps.max_callers);

    let mut siblings = siblings_all;
    siblings.truncate(caps.max_siblings);

    let mut tests: Vec<TestMatch> = test_callers
        .into_iter()
        .map(|m| TestMatch {
            path: m.path,
            line: m.line,
            test_name: m.calling_function,
        })
        .collect();
    tests.truncate(caps.max_tests);

    Ok(GrokResult {
        target,
        body,
        callees_internal,
        callees_external,
        callers,
        siblings,
        tests,
        total_callees_internal,
        total_callees_external,
        total_callers,
        total_siblings,
        total_tests,
    })
}

/// Slice the target's source body out of `content`. Caps at `max_lines` total
/// — when the body is longer, keeps the first 2/3 and last 1/3, separated by
/// an elided-line marker. Returns "" on degenerate ranges or `max_lines == 0`
/// (used by callers that want to suppress the body section entirely).
fn slice_body(content: &str, start_line: u32, end_line: u32, max_lines: usize) -> String {
    if start_line == 0 || end_line < start_line || max_lines == 0 {
        return String::new();
    }
    let start_idx = (start_line as usize).saturating_sub(1);
    let end_idx = end_line as usize;
    let lines: Vec<&str> = content
        .lines()
        .skip(start_idx)
        .take(end_idx.saturating_sub(start_idx))
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    if lines.len() <= max_lines {
        return lines.join("\n");
    }
    let head_n = max_lines.saturating_mul(2) / 3;
    let tail_n = max_lines.saturating_sub(head_n);
    let elided = lines.len() - head_n - tail_n;
    let mut out = String::with_capacity(content.len() / 4);
    for line in &lines[..head_n] {
        out.push_str(line);
        out.push('\n');
    }
    let _ = writeln!(
        out,
        "... ({elided} lines elided — use tilth_read for full body)"
    );
    for line in &lines[lines.len() - tail_n..] {
        out.push_str(line);
        out.push('\n');
    }
    out.pop(); // strip trailing newline
    out
}

// ---------------------------------------------------------------------------
// A3: formatting
// ---------------------------------------------------------------------------

/// Render a `GrokResult` as compact markdown — the agent-facing output.
///
/// Sections are skipped when empty. Pre-truncation totals are surfaced in the
/// section heading when capping happened (e.g. `## callers (5 of 23)`).
#[must_use]
pub fn format_grok(result: &GrokResult, scope: &Path) -> String {
    let mut out = String::new();
    let target_rel = display_rel(&result.target.path, scope);

    // Header
    let _ = writeln!(
        out,
        "# grok: {} [{}:{}]",
        result.target.name, target_rel, result.target.start_line
    );

    if result.target.other_def_count > 0 {
        let (suffix, verb) = if result.target.other_def_count == 1 {
            ("", "matches")
        } else {
            ("s", "match")
        };
        let _ = writeln!(
            out,
            "\n> ambiguous: {} other definition{} {} this name — re-run with --scope to narrow",
            result.target.other_def_count, suffix, verb,
        );
    }

    // Signature
    if let Some(sig) = &result.target.signature {
        out.push_str("\n## signature\n");
        out.push_str(sig);
        out.push('\n');
    }

    // Doc (1 line by default — caller can pass full doc if desired)
    if let Some(doc) = &result.target.doc {
        let first = doc.lines().next().unwrap_or("").trim();
        if !first.is_empty() {
            out.push_str("\n## doc\n");
            out.push_str(first);
            out.push('\n');
        }
    }

    // Body — the target's own source. Skipped when empty (degenerate range)
    // or when caps suppressed it.
    if !result.body.is_empty() {
        out.push_str("\n## body\n");
        out.push_str(&result.body);
        if !result.body.ends_with('\n') {
            out.push('\n');
        }
    }

    // Callees
    if !result.callees_internal.is_empty() || !result.callees_external.is_empty() {
        let internal_count = result.total_callees_internal;
        let external_count = result.total_callees_external;
        let _ = writeln!(
            out,
            "\n## callees ({internal_count} internal, {external_count} extern)"
        );
        for c in &result.callees_internal {
            let r = display_rel(&c.file, scope);
            let _ = writeln!(out, "{:<20} {}:{}", c.name, r, c.start_line);
        }
        for ext in &result.callees_external {
            let _ = writeln!(out, "{ext:<20} extern");
        }
        let shown = result.callees_internal.len() + result.callees_external.len();
        let total = internal_count + external_count;
        if shown < total {
            let _ = writeln!(out, "... and {} more", total - shown);
        }
    }

    // Callers
    if !result.callers.is_empty() {
        let _ = writeln!(
            out,
            "\n## callers ({})",
            count_label(result.callers.len(), result.total_callers)
        );
        for m in &result.callers {
            let r = display_rel(&m.path, scope);
            let loc = format!("{}:{}", r, m.line);
            let _ = writeln!(out, "{:<35} in {}()", loc, m.calling_function);
        }
        if result.callers.len() < result.total_callers {
            let _ = writeln!(
                out,
                "... and {} more",
                result.total_callers - result.callers.len()
            );
        }
    }

    // Siblings
    if !result.siblings.is_empty() {
        let _ = writeln!(out, "\n## siblings ({target_rel})");
        for s in &result.siblings {
            let label = s.signature.as_deref().unwrap_or_else(|| kind_label(s.kind));
            let _ = writeln!(
                out,
                "{:<24} [{}-{}]   {}",
                s.name, s.start_line, s.end_line, label
            );
        }
        if result.siblings.len() < result.total_siblings {
            let _ = writeln!(
                out,
                "... and {} more",
                result.total_siblings - result.siblings.len()
            );
        }
    }

    // Tests
    if !result.tests.is_empty() {
        let _ = writeln!(
            out,
            "\n## tests ({})",
            count_label(result.tests.len(), result.total_tests)
        );
        for t in &result.tests {
            let r = display_rel(&t.path, scope);
            let _ = writeln!(out, "{:<35} {}:{}", t.test_name, r, t.line);
        }
        if result.tests.len() < result.total_tests {
            let _ = writeln!(
                out,
                "... and {} more",
                result.total_tests - result.tests.len()
            );
        }
    }

    out
}

fn count_label(shown: usize, total: usize) -> String {
    if shown == total {
        total.to_string()
    } else {
        format!("{shown} of {total}")
    }
}

/// Render `path` relative to `scope` if possible, else the absolute path.
fn display_rel(path: &Path, scope: &Path) -> String {
    path.strip_prefix(scope)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn kind_label(kind: OutlineKind) -> &'static str {
    match kind {
        OutlineKind::Function => "fn",
        OutlineKind::Struct => "struct",
        OutlineKind::Class => "class",
        OutlineKind::Enum => "enum",
        OutlineKind::TypeAlias => "type",
        OutlineKind::Interface => "trait",
        OutlineKind::Constant => "const",
        OutlineKind::Variable | OutlineKind::ImmutableVariable => "var",
        _ => "",
    }
}

/// Is `callee` the target's own definition? (Recursive call resolved back to
/// the def we just looked up.) Same file + same start line uniquely identifies
/// the definition — function names alone aren't enough for languages that
/// allow overloading.
fn is_self_definition(
    callee: &ResolvedCallee,
    canonical_target: &Path,
    target: &ResolvedTarget,
) -> bool {
    callee.file == canonical_target && callee.start_line == target.start_line
}

/// Is this caller match a recursive call from inside the target's own body?
/// Both the call site and the enclosing function must match the target.
fn is_recursive_call_site(
    m: &CallerMatch,
    canonical_target: &Path,
    target: &ResolvedTarget,
) -> bool {
    m.path == canonical_target
        && m.line >= target.start_line
        && m.line <= target.end_line
        && m.calling_function == target.name
}

/// Collect siblings of the target: peer methods if the target is a method,
/// otherwise peer top-level definitions in the same file.
///
/// Skips imports/exports (noise) and the target itself. Sorted by:
/// functions/methods first, then alphabetical.
fn collect_siblings(entries: &[OutlineEntry], target: &ResolvedTarget) -> Vec<SiblingEntry> {
    let parent = entries.iter().find(|e| {
        e.children
            .iter()
            .any(|c| c.start_line == target.start_line && c.name == target.name)
    });

    let candidates: Vec<&OutlineEntry> = if let Some(p) = parent {
        p.children.iter().collect()
    } else {
        entries.iter().collect()
    };

    let mut out: Vec<SiblingEntry> = candidates
        .into_iter()
        .filter(|e| !matches!(e.kind, OutlineKind::Import | OutlineKind::Export))
        .filter(|e| !(e.start_line == target.start_line && e.name == target.name))
        .map(|e| SiblingEntry {
            name: e.name.clone(),
            kind: e.kind,
            start_line: e.start_line,
            end_line: e.end_line,
            signature: e.signature.clone(),
        })
        .collect();

    out.sort_by(|a, b| {
        let a_fn = matches!(a.kind, OutlineKind::Function);
        let b_fn = matches!(b.kind, OutlineKind::Function);
        b_fn.cmp(&a_fn).then_with(|| a.name.cmp(&b.name))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_entry(kind: OutlineKind, name: &str, start: u32, end: u32) -> OutlineEntry {
        OutlineEntry {
            kind,
            name: name.to_string(),
            start_line: start,
            end_line: end,
            signature: None,
            children: Vec::new(),
            doc: None,
        }
    }

    // -- parse_target_spec -----------------------------------------------

    #[test]
    fn parse_spec_bare_symbol() {
        assert_eq!(
            parse_target_spec("parse_unified_diff"),
            TargetSpec::Symbol("parse_unified_diff".to_string())
        );
    }

    #[test]
    fn parse_spec_path_line() {
        assert_eq!(
            parse_target_spec("src/diff/parse.rs:7"),
            TargetSpec::PathLine {
                path: PathBuf::from("src/diff/parse.rs"),
                line: 7,
            }
        );
    }

    #[test]
    fn parse_spec_type_path_is_symbol() {
        // `Box::new` must not parse as path:line — the right side isn't numeric.
        assert_eq!(
            parse_target_spec("Box::new"),
            TargetSpec::Symbol("Box::new".to_string())
        );
    }

    #[test]
    fn parse_spec_numeric_suffix_without_path_separator_is_symbol() {
        // `Foo:42` with no slash or dot in the left side is treated as a symbol —
        // it's almost certainly a method call or unusual name, not a path.
        assert_eq!(
            parse_target_spec("Foo:42"),
            TargetSpec::Symbol("Foo:42".to_string())
        );
    }

    #[test]
    fn parse_spec_trailing_colon_no_digits_is_symbol() {
        assert_eq!(
            parse_target_spec("foo:"),
            TargetSpec::Symbol("foo:".to_string())
        );
    }

    #[test]
    fn parse_spec_dotted_path_line() {
        // No directory but contains `.` — still a path:line.
        assert_eq!(
            parse_target_spec("parse.rs:99"),
            TargetSpec::PathLine {
                path: PathBuf::from("parse.rs"),
                line: 99,
            }
        );
    }

    // -- find_entry_at_line ----------------------------------------------

    #[test]
    fn line_lookup_finds_top_level_entry() {
        let entries = vec![
            make_entry(OutlineKind::Function, "foo", 10, 20),
            make_entry(OutlineKind::Function, "bar", 30, 40),
        ];
        let hit = find_entry_at_line(&entries, 15).expect("expected entry at line 15");
        assert_eq!(hit.name, "foo");
    }

    #[test]
    fn line_lookup_prefers_deepest_match() {
        let mut class = make_entry(OutlineKind::Class, "MyClass", 1, 50);
        class
            .children
            .push(make_entry(OutlineKind::Function, "method", 10, 25));
        let entries = vec![class];
        let hit = find_entry_at_line(&entries, 12).expect("expected method match");
        assert_eq!(hit.name, "method", "should pick child over parent");
    }

    #[test]
    fn line_lookup_returns_parent_when_no_child_match() {
        let mut class = make_entry(OutlineKind::Class, "MyClass", 1, 50);
        class
            .children
            .push(make_entry(OutlineKind::Function, "method", 10, 25));
        let entries = vec![class];
        let hit = find_entry_at_line(&entries, 30).expect("expected class match");
        assert_eq!(hit.name, "MyClass");
    }

    #[test]
    fn line_lookup_miss_returns_none() {
        let entries = vec![make_entry(OutlineKind::Function, "foo", 10, 20)];
        assert!(find_entry_at_line(&entries, 100).is_none());
    }

    // -- find_by_start_line ----------------------------------------------

    #[test]
    fn start_line_lookup_matches_exact_start() {
        let mut class = make_entry(OutlineKind::Class, "Outer", 1, 50);
        class
            .children
            .push(make_entry(OutlineKind::Function, "inner", 10, 25));
        let entries = vec![class];
        let hit = find_by_start_line(&entries, 10).expect("expected inner");
        assert_eq!(hit.name, "inner");
    }

    #[test]
    fn start_line_lookup_no_match_returns_none() {
        let entries = vec![make_entry(OutlineKind::Function, "foo", 10, 20)];
        assert!(find_by_start_line(&entries, 11).is_none());
    }

    // -- resolve_by_path_line — integration via tempdir ------------------

    fn write_fixture(dir: &Path, rel: &str, body: &str) -> PathBuf {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn resolve_by_path_line_finds_enclosing_function() {
        let tmp = tempfile::tempdir().unwrap();
        let body = "fn alpha() {\n    let x = 1;\n}\n\nfn beta() {\n    let y = 2;\n}\n";
        let path = write_fixture(tmp.path(), "src/a.rs", body);

        let (target, content, lang) = resolve_by_path_line(&path, 2).unwrap();
        assert_eq!(target.name, "alpha");
        assert_eq!(target.kind, OutlineKind::Function);
        assert_eq!(target.start_line, 1);
        assert_eq!(target.other_def_count, 0);
        assert!(
            content.contains("fn alpha"),
            "content should be the file body"
        );
        assert_eq!(lang, Lang::Rust);
    }

    #[test]
    fn resolve_by_path_line_returns_not_found_outside_any_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let body = "fn alpha() {}\n";
        let path = write_fixture(tmp.path(), "src/a.rs", body);
        let err = resolve_by_path_line(&path, 99).unwrap_err();
        assert!(matches!(err, TilthError::NotFound { .. }));
    }

    #[test]
    fn resolve_by_path_line_rejects_non_code_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_fixture(tmp.path(), "notes.txt", "hello\n");
        let err = resolve_by_path_line(&path, 1).unwrap_err();
        assert!(matches!(err, TilthError::InvalidQuery { .. }));
    }

    #[test]
    fn resolve_by_path_line_missing_file_is_io_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nope.rs");
        let err = resolve_by_path_line(&path, 1).unwrap_err();
        assert!(matches!(err, TilthError::IoError { .. }));
    }

    // -- resolve_target — full spec dispatch -----------------------------

    #[test]
    fn resolve_target_dispatches_path_line() {
        let tmp = tempfile::tempdir().unwrap();
        let body = "fn one() {}\n\nfn two() {\n    let x = 1;\n}\n";
        write_fixture(tmp.path(), "src/a.rs", body);

        // Relative path is joined with scope.
        let (target, _, _) = resolve_with_source("src/a.rs:3", tmp.path()).unwrap();
        assert_eq!(target.name, "two");
    }

    // -- collect_siblings ------------------------------------------------

    fn target_in_file(name: &str, start: u32, end: u32, path: &str) -> ResolvedTarget {
        ResolvedTarget {
            name: name.to_string(),
            path: PathBuf::from(path),
            start_line: start,
            end_line: end,
            kind: OutlineKind::Function,
            signature: None,
            doc: None,
            other_def_count: 0,
        }
    }

    #[test]
    fn siblings_top_level_skips_target_and_imports() {
        let entries = vec![
            make_entry(OutlineKind::Import, "std::fs", 1, 1),
            make_entry(OutlineKind::Function, "target", 5, 10),
            make_entry(OutlineKind::Function, "alpha", 12, 15),
            make_entry(OutlineKind::Function, "beta", 17, 20),
        ];
        let target = target_in_file("target", 5, 10, "src/a.rs");
        let sibs = collect_siblings(&entries, &target);
        let names: Vec<&str> = sibs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn siblings_methods_uses_parent_children() {
        let mut class = make_entry(OutlineKind::Class, "MyStruct", 1, 50);
        class
            .children
            .push(make_entry(OutlineKind::Function, "target", 5, 10));
        class
            .children
            .push(make_entry(OutlineKind::Function, "peer_a", 12, 15));
        class
            .children
            .push(make_entry(OutlineKind::Function, "peer_b", 17, 20));
        let entries = vec![
            class,
            make_entry(OutlineKind::Function, "unrelated_top_level", 60, 65),
        ];
        let target = target_in_file("target", 5, 10, "src/a.rs");
        let sibs = collect_siblings(&entries, &target);
        let names: Vec<&str> = sibs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["peer_a", "peer_b"],
            "should pick parent children, not top-level"
        );
    }

    #[test]
    fn siblings_functions_before_fields() {
        let entries = vec![
            make_entry(OutlineKind::Function, "target", 5, 10),
            make_entry(OutlineKind::Variable, "field_a", 12, 12),
            make_entry(OutlineKind::Function, "peer", 14, 18),
        ];
        let target = target_in_file("target", 5, 10, "src/a.rs");
        let sibs = collect_siblings(&entries, &target);
        let kinds: Vec<&str> = sibs
            .iter()
            .map(|s| match s.kind {
                OutlineKind::Function => "fn",
                OutlineKind::Variable => "var",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["fn", "var"], "functions sort first");
    }

    // -- grok end-to-end integration ------------------------------------

    #[test]
    fn grok_assembles_callees_callers_siblings_tests() {
        let tmp = tempfile::tempdir().unwrap();

        // Target file: defines `target_fn` which calls a helper, plus one sibling.
        let lib = "\
pub fn helper() -> u32 { 1 }

pub fn sibling_fn() {}

pub fn target_fn() {
    let _ = helper();
}
";
        write_fixture(tmp.path(), "src/lib.rs", lib);

        // Caller file: calls target_fn from a prod function.
        let caller = "\
use crate::target_fn;

pub fn caller_fn() {
    target_fn();
}
";
        write_fixture(tmp.path(), "src/caller.rs", caller);

        // Test file: calls target_fn from a test function.
        let tests = "\
use crate::target_fn;

#[test]
fn test_calls_target() {
    target_fn();
}
";
        write_fixture(tmp.path(), "src/lib.test.rs", tests);

        let bloom = BloomFilterCache::default();
        let result = grok("target_fn", tmp.path(), &bloom, GrokCaps::default()).unwrap();

        assert_eq!(result.target.name, "target_fn");
        assert_eq!(result.target.kind, OutlineKind::Function);

        let callee_names: Vec<&str> = result
            .callees_internal
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert!(
            callee_names.contains(&"helper"),
            "expected helper in callees, got {callee_names:?}"
        );

        let caller_names: Vec<&str> = result
            .callers
            .iter()
            .map(|c| c.calling_function.as_str())
            .collect();
        assert!(
            caller_names.contains(&"caller_fn"),
            "expected caller_fn in callers, got {caller_names:?}"
        );

        let sibling_names: Vec<&str> = result.siblings.iter().map(|s| s.name.as_str()).collect();
        assert!(
            sibling_names.contains(&"sibling_fn"),
            "expected sibling_fn in siblings, got {sibling_names:?}"
        );
        assert!(
            !sibling_names.contains(&"target_fn"),
            "target should not list itself as a sibling"
        );

        let test_names: Vec<&str> = result.tests.iter().map(|t| t.test_name.as_str()).collect();
        assert!(
            test_names.contains(&"test_calls_target"),
            "expected test_calls_target in tests, got {test_names:?}"
        );

        // Tests and callers must not overlap.
        let caller_paths: HashSet<&Path> =
            result.callers.iter().map(|c| c.path.as_path()).collect();
        for t in &result.tests {
            assert!(
                !caller_paths.contains(t.path.as_path()),
                "test file leaked into callers section"
            );
        }
    }

    #[test]
    fn format_grok_renders_full_sections() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = "\
pub fn helper() -> u32 { 1 }
pub fn peer() {}

/// Doc line one.
/// Doc line two.
pub fn target() {
    let _ = helper();
}
";
        write_fixture(tmp.path(), "src/lib.rs", lib);
        let bloom = BloomFilterCache::default();
        let result = grok("target", tmp.path(), &bloom, GrokCaps::default()).unwrap();
        let rendered = format_grok(&result, tmp.path());

        assert!(
            rendered.starts_with("# grok: target [src/lib.rs:"),
            "actual: {rendered}"
        );
        assert!(rendered.contains("## signature"), "actual: {rendered}");
        assert!(rendered.contains("## callees"), "actual: {rendered}");
        assert!(rendered.contains("helper"), "actual: {rendered}");
        assert!(rendered.contains("## siblings"), "actual: {rendered}");
        assert!(rendered.contains("peer"), "actual: {rendered}");
        // No callers / tests in this fixture — sections must be omitted.
        assert!(!rendered.contains("## callers"), "actual: {rendered}");
        assert!(!rendered.contains("## tests"), "actual: {rendered}");
    }

    #[test]
    fn format_grok_marks_ambiguous_target() {
        let target = ResolvedTarget {
            name: "foo".into(),
            path: PathBuf::from("src/a.rs"),
            start_line: 5,
            end_line: 8,
            kind: OutlineKind::Function,
            signature: Some("fn foo()".into()),
            doc: None,
            other_def_count: 3,
        };
        let result = GrokResult {
            target,
            body: String::new(),
            callees_internal: Vec::new(),
            callees_external: Vec::new(),
            callers: Vec::new(),
            siblings: Vec::new(),
            tests: Vec::new(),
            total_callees_internal: 0,
            total_callees_external: 0,
            total_callers: 0,
            total_siblings: 0,
            total_tests: 0,
        };
        let out = format_grok(&result, Path::new("."));
        assert!(out.contains("ambiguous: 3 other definitions match"));
    }

    #[test]
    fn format_grok_shows_truncation_tail() {
        let target = ResolvedTarget {
            name: "f".into(),
            path: PathBuf::from("src/a.rs"),
            start_line: 1,
            end_line: 2,
            kind: OutlineKind::Function,
            signature: None,
            doc: None,
            other_def_count: 0,
        };
        // Manually construct a result where callers count > total displayed.
        let result = GrokResult {
            target,
            body: String::new(),
            callees_internal: Vec::new(),
            callees_external: Vec::new(),
            callers: Vec::new(),
            siblings: vec![SiblingEntry {
                name: "peer".into(),
                kind: OutlineKind::Function,
                start_line: 5,
                end_line: 8,
                signature: None,
            }],
            tests: Vec::new(),
            total_callees_internal: 0,
            total_callees_external: 0,
            total_callers: 0,
            total_siblings: 17,
            total_tests: 0,
        };
        let out = format_grok(&result, Path::new("."));
        assert!(out.contains("... and 16 more"));
    }

    // -- slice_body --------------------------------------------------------

    #[test]
    fn slice_body_returns_full_body_when_under_cap() {
        let content = "fn foo() {\n    let x = 1;\n    x + 1\n}\n";
        // start_line=1, end_line=4, cap=60 → full body
        let body = slice_body(content, 1, 4, 60);
        assert_eq!(body, "fn foo() {\n    let x = 1;\n    x + 1\n}");
    }

    #[test]
    fn slice_body_elides_long_bodies() {
        let lines: Vec<String> = (1..=30).map(|i| format!("line {i}")).collect();
        let content = lines.join("\n");
        let body = slice_body(&content, 1, 30, 9);
        // head_n = 9 * 2 / 3 = 6, tail_n = 3, elided = 30 - 6 - 3 = 21
        assert!(body.contains("line 1\n"));
        assert!(body.contains("line 6\n"));
        assert!(body.contains("21 lines elided"));
        assert!(body.contains("line 28\n"));
        assert!(body.contains("line 30"));
        assert!(!body.contains("line 7\n"), "head must stop at line 6");
        assert!(!body.contains("line 27\n"), "tail must start at line 28");
    }

    #[test]
    fn slice_body_handles_degenerate_range() {
        assert_eq!(slice_body("anything", 0, 5, 60), "");
        assert_eq!(slice_body("anything", 5, 3, 60), "");
    }

    #[test]
    fn slice_body_zero_cap_suppresses_body() {
        // max_lines=0 = caller wants to suppress the body section entirely.
        // Without this guard, the function would emit just the elision marker.
        assert_eq!(slice_body("a\nb\nc", 1, 3, 0), "");
    }

    #[test]
    fn slice_body_clamps_when_end_past_eof() {
        let content = "a\nb\nc";
        // Asking for lines 1..=10 against a 3-line file → should still return what exists.
        let body = slice_body(content, 1, 10, 60);
        assert_eq!(body, "a\nb\nc");
    }

    // -- grok body integration --------------------------------------------

    #[test]
    fn grok_includes_target_body() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = "\
pub fn target_fn() -> u32 {
    let x = 42;
    x + 1
}
";
        write_fixture(tmp.path(), "src/lib.rs", lib);
        let bloom = BloomFilterCache::default();
        let result = grok("target_fn", tmp.path(), &bloom, GrokCaps::default()).unwrap();
        assert!(
            result.body.contains("let x = 42"),
            "body should contain target source, got {:?}",
            result.body
        );
        let rendered = format_grok(&result, tmp.path());
        assert!(
            rendered.contains("## body"),
            "format must surface body section"
        );
        assert!(rendered.contains("let x = 42"));
    }

    #[test]
    fn grok_caps_truncate_results() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = "\
pub fn target() {
    a(); b(); c(); d(); e(); f(); g(); h();
}

fn a() {}
fn b() {}
fn c() {}
fn d() {}
fn e() {}
fn f() {}
fn g() {}
fn h() {}
";
        write_fixture(tmp.path(), "src/lib.rs", lib);

        let bloom = BloomFilterCache::default();
        let caps = GrokCaps {
            max_callees: 3,
            max_callers: 5,
            max_siblings: 2,
            max_tests: 8,
            max_body_lines: 60,
        };
        let result = grok("target", tmp.path(), &bloom, caps).unwrap();
        assert_eq!(result.callees_internal.len(), 3, "callees capped");
        assert_eq!(result.siblings.len(), 2, "siblings capped");
        assert!(
            result.total_callees_internal >= 8,
            "pre-cap total preserved"
        );
        assert!(result.total_siblings >= 8);
    }
}
