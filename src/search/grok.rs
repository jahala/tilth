//! Grok orchestrator: collapse the "search → expand → search-callers → read-context"
//! dance into one structured response. A1 ships target resolution; A2 assembles
//! callees/callers/siblings/tests; A3 will format.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::search::search_symbol_raw;
use tilth_core::error::TilthError;
use tilth_core::index::bloom::BloomFilterCache;
use tilth_core::lang::detect_file_type;
use tilth_core::lang::outline::get_outline_entries;
use tilth_core::search::callees::{extract_callee_names, resolve_callees, ResolvedCallee};
use tilth_core::search::callers::{find_callers_batch, CallerMatch};
use tilth_core::types::{is_test_file, FileType, Lang, OutlineEntry, OutlineKind};

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

/// Resolve a symbol target to its top-ranked definition. A qualified target
/// (`Type::method`, `Type.method`) resolves among the definitions its owner
/// holds. When the owner holds none, the error lists the definitions of the
/// name there are, so the next call can name one.
fn resolve_by_name(name: &str, scope: &Path) -> Result<(ResolvedTarget, String, Lang), TilthError> {
    // Only the top-ranked definition is used, so the short match list is
    // enough; the count of the others comes from the exact total. Grok's own
    // caps come from `GrokCaps`.
    let result = search_symbol_raw(name, scope, None, false)?;
    let Some(top) = result.matches.iter().find(|m| m.is_definition) else {
        let suggestion = (!result.other_owners.is_empty())
            .then(|| crate::search::owner_candidates(name, &result.other_owners, scope));
        return Err(TilthError::NotFound {
            path: PathBuf::from(name),
            suggestion,
        });
    };
    // A doc heading that names the symbol is no rival definition.
    let other_def_count = result.code_definitions.saturating_sub(1);
    let (start, _end) = top.def_range.ok_or_else(|| TilthError::ParseError {
        path: top.path.clone(),
        reason: format!("definition match for `{name}` had no def_range"),
    })?;
    let def_name = top.def_name.clone().unwrap_or_else(|| name.to_string());
    enrich_from_outline(top.path.clone(), start, def_name, name, other_def_count)
}

fn resolve_by_path_line(
    path: &Path,
    line: u32,
) -> Result<(ResolvedTarget, String, Lang), TilthError> {
    let (content, lang) = read_code_file(path)?;
    let entries = get_outline_entries(&content, lang);
    // A decorator or attribute line belongs to the definition it decorates,
    // though that definition's outline entry may start below it.
    let line = tilth_core::lang::treesitter::decorated_definition_line(&content, lang, line)
        .unwrap_or(line);
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
    // Stat before judging the extension: a missing path must report
    // file-not-found, not "not a code file", whatever its suffix.
    let meta = fs::metadata(path).map_err(|e| TilthError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;
    let FileType::Code(lang) = detect_file_type(path) else {
        return Err(TilthError::InvalidQuery {
            query: path.display().to_string(),
            reason: "not a code file — grok needs source code".to_string(),
        });
    };
    // The binary probe below only inspects the first 512B, so a clean head
    // followed by megabytes of garbage would still reach the lossy decode and
    // the outline parser. Refuse on size first, before reading any bytes.
    let cap = crate::read::full_read_size_cap();
    if meta.len() > cap {
        return Err(TilthError::InvalidQuery {
            query: path.display().to_string(),
            reason: format!(
                "file too large for grok ({}B > {cap}B cap) — use tilth_search \"<symbol>\" for matches in this file",
                meta.len()
            ),
        });
    }
    let bytes = tilth_core::source::read(path).map_err(|e| TilthError::IoError {
        path: path.to_path_buf(),
        source: e,
    })?;
    // Match the read path: refuse binary content instead of letting a strict
    // UTF-8 decode surface a raw "stream did not contain valid UTF-8" error.
    if tilth_core::lang::detection::is_binary(&bytes) {
        return Err(TilthError::InvalidQuery {
            query: path.display().to_string(),
            reason: "binary or non-text file (null bytes in first 512B) — grok needs source; \
                     use tilth_search \"<symbol>\" for matches in this file"
                .to_string(),
        });
    }
    // Try the strict conversion first: it takes ownership of the buffer, so
    // the (near-universal) valid-UTF-8 case decodes without a second copy.
    let mut content = String::from_utf8(bytes)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
    let bom = content.len() - tilth_core::source::strip_bom(&content).len();
    content.drain(..bom);
    let lang = tilth_core::lang::refine_lang(lang, path, &content);
    Ok((content, lang))
}

/// Read the file at `path`, find the outline entry named `name` that starts
/// at `start_line` (or else any entry starting there, or the deepest one
/// enclosing it), and convert to `ResolvedTarget`. `query` is what the user
/// asked for, which a refusal names.
fn enrich_from_outline(
    path: PathBuf,
    start_line: u32,
    name: String,
    query: &str,
    other_def_count: usize,
) -> Result<(ResolvedTarget, String, Lang), TilthError> {
    // On the symbol route the user asked for `query`, not the path the search
    // resolved it to — a refusal must name their query, not our detour.
    let (content, lang) = read_code_file(&path).map_err(|e| match e {
        TilthError::InvalidQuery { reason, .. } => TilthError::InvalidQuery {
            query: query.to_string(),
            reason,
        },
        other => other,
    })?;
    let entries = get_outline_entries(&content, lang);
    // A definition's range opens with the decorators beside it, but its
    // outline entry starts on its own first line, below them.
    let start_line =
        tilth_core::lang::treesitter::decorated_definition_line(&content, lang, start_line)
            .unwrap_or(start_line);
    // A one-line class and its first method start on the same line; the
    // name tells them apart.
    let entry = find_named_at(&entries, start_line, &name)
        .or_else(|| find_by_start_line(&entries, start_line))
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

/// Walk the outline tree and return the deepest entry named `name` whose
/// `start_line == line`.
fn find_named_at<'a>(
    entries: &'a [OutlineEntry],
    line: u32,
    name: &str,
) -> Option<&'a OutlineEntry> {
    entries.iter().find_map(|e| {
        find_named_at(&e.children, line, name)
            .or_else(|| (e.start_line == line && e.name == name).then_some(e))
    })
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
// Body-dedup constants.
// ---------------------------------------------------------------------------

/// Skip dedup for bodies at or below this line count. A small body is already
/// cheap; degrading it to a pointer saves no meaningful tokens and loses
/// signal the agent might need.
const BODY_DEGRADE_THRESHOLD: usize = 15;

/// Maximum body line count for the wrapper to qualify for thin-wrapper
/// auto-expansion. Bodies at or below this count are considered delegating
/// wrappers rather than substantive implementations.
const WRAPPER_MAX_BODY_LINES: usize = 10;

/// Number of body lines to keep after the signature when degrading. Enough
/// to anchor the agent's mental model; few enough to actually save tokens.
const BODY_PREVIEW_LINES: usize = 3;

/// Read a thin-wrapper's single delegate callee for inline expansion.
///
/// Reuses the already-loaded `target_content` when the callee lives in the
/// target's own file. For a cross-file callee it reads the file, but only when
/// it is at or below `cap` bytes — a delegating wrapper should point at an
/// ordinary function, not pull a huge or generated file into the bundle. Returns
/// `None` on an oversized, missing, or unreadable file (expansion is silently
/// skipped). `cap` is a parameter so the bound is exercisable in tests.
fn read_delegate_content(
    callee_file: &Path,
    target_path: &Path,
    target_content: &str,
    cap: u64,
) -> Option<String> {
    if callee_file == target_path {
        Some(target_content.to_string())
    } else if std::fs::metadata(callee_file).is_ok_and(|m| m.len() <= cap) {
        tilth_core::source::read_source(callee_file).ok()
    } else {
        None
    }
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
    /// When the target is a thin wrapper (small body delegating to exactly one
    /// internal callee with no external callees), this holds that callee and
    /// its sliced body so the formatter can render it inline. `None` otherwise.
    pub delegate_body: Option<(ResolvedCallee, String)>,
    /// Code files the caller walk skipped for size although they name the
    /// target.
    pub skipped_large: Vec<PathBuf>,
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
    session: &crate::session::Session,
    caps: GrokCaps,
) -> Result<GrokResult, TilthError> {
    let (target, content, lang) = resolve_with_source(target_spec, scope)?;
    let entries = get_outline_entries(&content, lang);

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

    // Capture the sole internal callee before the caps below truncate `resolved`,
    // so thin-wrapper expansion stays correct independent of `caps.max_callees`
    // (which could be 0). `Some` here iff there is exactly one internal callee.
    let lone_internal_callee: Option<ResolvedCallee> = if total_callees_internal == 1 {
        resolved.first().cloned()
    } else {
        None
    };

    // --- Callers + tests (one walk, partitioned by is_test_file) ----------
    // Both are counted in full. The ones kept are spread across files: every
    // file's first call site, by path, before any file's second, so a few
    // shown callers name as many places as they can. Each file offers its
    // first ones by line with their rank in the file, and they are merged
    // into the running first ones as files arrive; (rank, path, line) is a
    // total order, so the result does not depend on arrival order. Only the
    // capped number are held at once, since each holds its file's text.
    let symbols: HashSet<String> = std::iter::once(target.name.clone()).collect();
    let spread = |a: &(usize, CallerMatch), b: &(usize, CallerMatch)| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.path.cmp(&b.1.path))
            .then_with(|| a.1.line.cmp(&b.1.line))
    };
    let split: std::sync::Mutex<CallerSplit> = std::sync::Mutex::new(CallerSplit::default());
    let walk = find_callers_batch(&symbols, scope, bloom, None, None, |sites| {
        let mut file = CallerSplit::default();
        for (_, m) in sites {
            if is_recursive_call_site(&m, &canonical_target, &target) {
                continue;
            }
            if !is_test_file(&m.path) && !m.in_test {
                if file.callers.len() < caps.max_callers {
                    file.callers.push((file.total_callers, m));
                }
                file.total_callers += 1;
            } else {
                if file.tests.len() < caps.max_tests {
                    file.tests.push((file.total_tests, m));
                }
                file.total_tests += 1;
            }
        }
        let mut all = split
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        all.total_callers += file.total_callers;
        all.total_tests += file.total_tests;
        all.callers.extend(file.callers);
        all.callers.sort_by(spread);
        all.callers.truncate(caps.max_callers);
        all.tests.extend(file.tests);
        all.tests.sort_by(spread);
        all.tests.truncate(caps.max_tests);
    })?;
    let all = split
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let total_callers = all.total_callers;
    let total_tests = all.total_tests;
    let prod_callers: Vec<CallerMatch> = all.callers.into_iter().map(|(_, m)| m).collect();
    let test_callers: Vec<CallerMatch> = all.tests.into_iter().map(|(_, m)| m).collect();

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

    // Slice the body only once the fallible work above has succeeded: the
    // slice claims the session's dedup slot for this target, and a claim made
    // before a `?` would mark a body that the caller never received.
    let body = body_with_dedup(&target, &content, session, caps.max_body_lines);

    // Record this expansion so a same-session repeat-grok of the same target
    // degrades to a preview rather than re-inlining the full body. Bodies at
    // or below the degrade threshold never reach the claim inside
    // `body_with_dedup`, so this is what marks them for `tilth_search`'s own
    // dedup. Silently skipped on metadata failure — the next call will just
    // re-inline normally.
    if let Ok(mtime) = std::fs::metadata(&target.path).and_then(|md| md.modified()) {
        session.record_expand(&target.path, target.start_line, mtime);
    }

    // --- Thin-wrapper auto-expansion --------------------------------------
    // Predicate: the target's definition span is short (≤ WRAPPER_MAX_BODY_LINES),
    // there is exactly one resolved internal callee (pre-cap total), and
    // there are zero external callees. When this fires, slice the single
    // callee's body and attach it to the result for the formatter to render.
    // Measure the true definition span, not `body` — `body` may be a dedup-degraded
    // preview on a re-grok, which would otherwise make a large function look thin.
    let delegate_body = if target.start_line > 0
        && (target.end_line.saturating_sub(target.start_line) as usize + 1)
            <= WRAPPER_MAX_BODY_LINES
        && total_callees_internal == 1
        && total_callees_external == 0
    {
        // `lone_internal_callee` is the pre-cap single callee, so it is `Some`
        // here whatever `caps.max_callees` is. The `start_line > 0` guard skips a
        // degenerate/unresolved target (line numbers are 1-based; 0 means
        // unresolved), mirroring `slice_body`.
        lone_internal_callee.as_ref().and_then(|callee| {
            let callee_content = read_delegate_content(
                &callee.file,
                &target.path,
                &content,
                tilth_core::source::MAX_FILE_SIZE,
            );
            callee_content.map(|cc| {
                // Build a minimal ResolvedTarget so body_with_dedup can handle dedup.
                let callee_target = ResolvedTarget {
                    name: callee.name.clone(),
                    path: callee.file.clone(),
                    start_line: callee.start_line,
                    end_line: callee.end_line,
                    kind: OutlineKind::Function,
                    signature: callee.signature.clone(),
                    doc: None,
                    other_def_count: 0,
                };
                let sliced = body_with_dedup(&callee_target, &cc, session, caps.max_body_lines);
                // Record the callee expansion so repeat-grok of the callee
                // degrades correctly (same protocol as the target recording above).
                if let Ok(mtime) = std::fs::metadata(&callee.file).and_then(|md| md.modified()) {
                    session.record_expand(&callee.file, callee.start_line, mtime);
                }
                (callee.clone(), sliced)
            })
        })
    } else {
        None
    };

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
        delegate_body,
        skipped_large: walk.skipped_large,
    })
}

/// Slice the target's source body out of `content`. Caps at `max_lines` total
/// — when the body is longer, keeps the first 2/3 and last 1/3, separated by
/// an elided-line marker. Returns "" on degenerate ranges or `max_lines == 0`
/// (used by callers that want to suppress the body section entirely).
/// Return either the full body slice (un-degraded) or a degraded preview
/// when the body was already shown in this session AND the file is unchanged.
///
/// Degraded format:
///   <full body line 1>           ← the signature line is part of the body slice
///   <up to BODY_PREVIEW_LINES more lines>
///   // [{N}-line body shown earlier — {path}:{line}]
///
/// Skips degradation when the body is at or below `BODY_DEGRADE_THRESHOLD` —
/// the original is already short, degrading saves no meaningful tokens.
/// Returns the un-degraded body whenever metadata lookup fails, the file's
/// mtime has changed since the recorded expansion, or this is the first time
/// we've seen this target. That's the safe fallback.
fn body_with_dedup(
    target: &ResolvedTarget,
    content: &str,
    session: &crate::session::Session,
    max_body_lines: usize,
) -> String {
    let full = slice_body(content, target.start_line, target.end_line, max_body_lines);
    let line_count = full.lines().count();
    if line_count <= BODY_DEGRADE_THRESHOLD {
        return full;
    }
    let Some(current_mtime) = std::fs::metadata(&target.path)
        .ok()
        .and_then(|md| md.modified().ok())
    else {
        return full;
    };
    if !session.claim_expand(&target.path, target.start_line, current_mtime) {
        return full;
    }
    let mut preview = String::new();
    for line in full.lines().take(BODY_PREVIEW_LINES + 1) {
        preview.push_str(line);
        preview.push('\n');
    }
    let _ = write!(
        preview,
        "// [{line_count}-line body shown earlier — {}:{}]",
        target.path.display(),
        target.start_line
    );
    preview
}

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

    // Delegate body — rendered immediately after the wrapper's own body
    // when the thin-wrapper predicate fired during assembly.
    if let Some((callee, callee_body)) = &result.delegate_body {
        let callee_rel = display_rel(&callee.file, scope);
        let _ = writeln!(
            out,
            "\n## delegate body — {}:{}",
            callee_rel, callee.start_line
        );
        out.push_str(callee_body);
        if !callee_body.ends_with('\n') {
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
        // Group internal callees by file so the agent sees the cross-file
        // dependency structure at a glance. BTreeMap keeps file order stable.
        let mut by_file: std::collections::BTreeMap<&Path, Vec<&ResolvedCallee>> =
            std::collections::BTreeMap::new();
        for c in &result.callees_internal {
            by_file.entry(c.file.as_path()).or_default().push(c);
        }
        for (file, mut callees) in by_file {
            callees.sort_by_key(|c| c.start_line);
            let _ = writeln!(out, "  {}", display_rel(file, scope));
            for c in callees {
                let sig = c.signature.as_deref().unwrap_or("");
                let _ = writeln!(
                    out,
                    "    {:<20} [{}-{}]   {}",
                    c.name, c.start_line, c.end_line, sig
                );
            }
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
        // Group callers by file — collapses the common case of multiple
        // call sites in the same file under one header.
        let mut by_file: std::collections::BTreeMap<&Path, Vec<&CallerMatch>> =
            std::collections::BTreeMap::new();
        for m in &result.callers {
            by_file.entry(m.path.as_path()).or_default().push(m);
        }
        for (file, mut callers) in by_file {
            callers.sort_by_key(|m| m.line);
            let _ = writeln!(out, "  {}", display_rel(file, scope));
            for m in callers {
                let _ = writeln!(out, "    [{}]   in {}()", m.line, m.calling_function);
            }
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

    // The walk looked for the target's bare name, whatever qualified it.
    let held = crate::search::names_phrase([result.target.name.as_str()]);
    if let Some(sentence) = crate::search::skipped_files_note(&result.skipped_large, &held, scope) {
        let _ = writeln!(out, "\n\n> {sentence}");
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

/// A target's callers split into production code and tests, counted in
/// full, with only the first ones kept up to the grok caps. Each kept site
/// carries its rank among its file's sites of the same kind.
#[derive(Default)]
struct CallerSplit {
    total_callers: usize,
    total_tests: usize,
    callers: Vec<(usize, CallerMatch)>,
    tests: Vec<(usize, CallerMatch)>,
}

/// Is this caller match a recursive call from inside the target's own body?
/// Both the call site and the enclosing function must match the target.
fn is_recursive_call_site(
    m: &CallerMatch,
    canonical_target: &Path,
    target: &ResolvedTarget,
) -> bool {
    // Edge case: a legitimately distinct same-named call site nested inside the
    // target body (e.g. a recursive closure with the same name) would also be
    // filtered here. Acceptable tradeoff — true self-recursion is the common case.
    // The walker's paths keep the scope's spelling, which is the target's
    // own path when the scope sits behind a symlink. The label carries the
    // owner (`Walker.walk`); the target's name does not.
    (m.path == canonical_target || m.path == target.path)
        && m.line >= target.start_line
        && m.line <= target.end_line
        && tilth_core::search::scope::split_qualified(&m.calling_function).1 == target.name
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
            local_name: None,
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

    // -- qualified names ---------------------------------------------------

    fn grok_target(dir: &Path, spec: &str) -> Result<ResolvedTarget, TilthError> {
        let bloom = BloomFilterCache::default();
        let session = crate::session::Session::default();
        grok(spec, dir, &bloom, &session, GrokCaps::default()).map(|r| r.target)
    }

    /// A qualified target resolves to the definition its owner holds, with
    /// either separator, and not to another owner's definition of the name.
    #[test]
    fn grok_resolves_a_qualified_name_by_its_owner() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(
            tmp.path(),
            "m.rs",
            "pub struct A;\npub struct B;\nimpl A {\n    pub fn run(&self) {}\n}\nimpl B {\n    pub fn run(&self) {}\n}\n",
        );
        write_fixture(
            tmp.path(),
            "q.ts",
            "class Target { foo() { return 1; } }\nclass Other { foo() { return 2; } }\n",
        );
        write_fixture(
            tmp.path(),
            "store.py",
            "class Store:\n    def get(self, key):\n        return key\n\nclass Cache:\n    def get(self, key):\n        return 1\n",
        );
        write_fixture(
            tmp.path(),
            "render.go",
            "package render\n\nfunc (r JSON) Render() error { return nil }\n\nfunc (r XML) Render() error { return nil }\n",
        );
        for (spec, file, line, name) in [
            ("B::run", "m.rs", 7, "run"),
            ("B.run", "m.rs", 7, "run"),
            ("A::run", "m.rs", 4, "run"),
            ("Other.foo", "q.ts", 2, "foo"),
            ("Target.foo", "q.ts", 1, "foo"),
            ("Cache.get", "store.py", 6, "get"),
            ("XML.Render", "render.go", 5, "Render"),
        ] {
            let target = grok_target(tmp.path(), spec).unwrap();
            assert_eq!(
                (
                    target.path.file_name().unwrap().to_str().unwrap(),
                    target.start_line,
                    target.name.as_str()
                ),
                (file, line, name),
                "{spec}"
            );
            assert_eq!(target.other_def_count, 0, "{spec} has one definition");
        }
    }

    /// A qualifier naming a package, a module or `Self`/`self`/`this` finds
    /// the definition by its name, as grok's bare-name retry did before
    /// #296, and an Elixir dotted module is found by its full name.
    #[test]
    fn grok_resolves_names_qualified_by_a_module_or_self() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(
            tmp.path(),
            "gin.go",
            "package gin\n\nfunc New() int {\n\treturn 1\n}\n",
        );
        write_fixture(
            tmp.path(),
            "runner.rs",
            "pub struct Runner;\nimpl Runner {\n    pub fn new() -> Self {\n        Runner\n    }\n    pub fn run(&self) {}\n}\n",
        );
        write_fixture(tmp.path(), "symbol.rs", "pub fn search() {}\n");
        write_fixture(
            tmp.path(),
            "accounts.ex",
            "defmodule MyApp.Accounts do\n  def get_user(id), do: id\nend\n",
        );
        for (spec, file, line) in [
            ("gin.New", "gin.go", 3),
            ("Self::new", "runner.rs", 3),
            ("self.run", "runner.rs", 6),
            ("crate::search::symbol::search", "symbol.rs", 1),
            ("MyApp.Accounts", "accounts.ex", 1),
            ("MyApp.Accounts.get_user", "accounts.ex", 2),
        ] {
            let target = grok_target(tmp.path(), spec).unwrap_or_else(|e| panic!("{spec}: {e}"));
            assert_eq!(
                (
                    target.path.file_name().unwrap().to_str().unwrap(),
                    target.start_line
                ),
                (file, line),
                "{spec}"
            );
        }
    }

    /// grok and search mark an expanded definition in the session under one
    /// line, so each shows the other's body as shown earlier. A definition
    /// whose annotations, decorators or return type sit above its name line
    /// was marked under two lines and inlined twice.
    #[test]
    fn grok_and_search_share_the_expansion_of_an_annotated_definition() {
        // Twenty lines in each body, past the length grok degrades on repeat.
        let lines = |indent: &str, end: &str| -> String {
            (0..20).fold(String::new(), |mut out, i| {
                let _ = writeln!(out, "{indent}v{i} = {i}{end}");
                out
            })
        };
        let body = lines("        int ", ";");
        let java = format!("class Svc {{\n    @Override\n    public String run() {{\n{body}        return \"x\";\n    }}\n}}\n");
        let py_body = lines("    ", "");
        let py = format!("@app.get(\"/r\")\ndef route():\n{py_body}    return 1\n");
        let c_body = lines("  int ", ";");
        let c = format!("static int\nrun_c(void)\n{{\n{c_body}  return 0;\n}}\n");
        for (file, fixture, query) in [
            ("Svc.java", java.as_str(), "run"),
            ("app.py", py.as_str(), "route"),
            ("r.c", c.as_str(), "run_c"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            write_fixture(tmp.path(), file, fixture);
            let bloom = BloomFilterCache::default();
            let cache = tilth_core::cache::OutlineCache::new();
            let search = |session: &crate::session::Session| {
                crate::search::search_symbol_expanded(
                    query,
                    tmp.path(),
                    &cache,
                    session,
                    &bloom,
                    1,
                    None,
                    None,
                    false,
                    None,
                )
                .unwrap()
            };

            let session = crate::session::Session::default();
            grok(query, tmp.path(), &bloom, &session, GrokCaps::default()).unwrap();
            let out = search(&session);
            assert!(
                out.contains("[shown earlier]"),
                "{file}: grok, then search: {out}"
            );

            let session = crate::session::Session::default();
            search(&session);
            let again = grok(query, tmp.path(), &bloom, &session, GrokCaps::default()).unwrap();
            assert!(
                again.body.contains("body shown earlier"),
                "{file}: search, then grok: {}",
                again.body
            );
        }
    }

    /// A qualified target whose owner holds no definition of the name is not
    /// found, and the error names the definitions there are.
    #[test]
    fn grok_names_the_owners_when_the_qualifier_matches_none() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(
            tmp.path(),
            "m.rs",
            "pub struct A;\nimpl A {\n    pub fn run(&self) {}\n}\npub fn run() {}\n",
        );
        match grok_target(tmp.path(), "Gamma::run").unwrap_err() {
            TilthError::NotFound { path, suggestion } => {
                assert_eq!(path, PathBuf::from("Gamma::run"));
                assert_eq!(suggestion.as_deref(), Some("A::run (m.rs:3), run (m.rs:5)"));
            }
            other => panic!("expected NotFound, got {other}"),
        }
    }

    /// `path:line` on a decorator or attribute line resolves to the
    /// definition it decorates, not to the class or module around it.
    #[test]
    fn grok_path_line_on_a_decorator_resolves_to_the_decorated_definition() {
        let tmp = tempfile::tempdir().unwrap();
        let ts = write_fixture(
            tmp.path(),
            "service.ts",
            "@Injectable()\nclass Service {\n  @First()\n  @Second()\n  method() {\n    return 1;\n  }\n}\n",
        );
        let py = write_fixture(
            tmp.path(),
            "s.py",
            "class S:\n    @property\n    @other\n    def run(self):\n        return 1\n",
        );
        let rs = write_fixture(
            tmp.path(),
            "t.rs",
            "mod t {\n    #[test]\n    #[ignore]\n    fn case() {\n        go();\n    }\n}\n",
        );
        for (path, line, name) in [
            (&ts, 1, "Service"),
            (&ts, 3, "method"),
            (&ts, 4, "method"),
            (&py, 2, "run"),
            (&py, 3, "run"),
            (&rs, 2, "case"),
            (&rs, 3, "case"),
        ] {
            let (target, _, _) = resolve_by_path_line(path, line).unwrap();
            assert_eq!(target.name, name, "{}:{line}", path.display());
        }
    }

    /// A recursive method call is the method calling itself, not a caller,
    /// though its label carries the owner (`Walker.walk`).
    #[test]
    fn grok_does_not_list_a_recursive_method_as_its_own_caller() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(
            tmp.path(),
            "w.ts",
            "class Walker {\n  walk(n: number): number {\n    return n ? this.walk(n - 1) : 0;\n  }\n}\n\
             export function start(w: Walker) { return w.walk(3); }\n",
        );
        let bloom = BloomFilterCache::default();
        let session = crate::session::Session::default();
        let result = grok(
            "Walker.walk",
            tmp.path(),
            &bloom,
            &session,
            GrokCaps::default(),
        )
        .unwrap();
        let callers: Vec<&str> = result
            .callers
            .iter()
            .map(|c| c.calling_function.as_str())
            .collect();
        assert_eq!(callers, ["start"]);
        assert_eq!(result.total_callers, 1);
    }

    /// Many callers in test files fill the tests section; the callers section
    /// still lists the production callers.
    #[test]
    fn grok_keeps_its_callers_when_test_callers_outnumber_them() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(
            tmp.path(),
            "src/lib.rs",
            "pub fn target_fn() -> u32 { 1 }\n",
        );
        let mut prod = String::new();
        for i in 0..75 {
            let _ = writeln!(prod, "pub fn c{i}() -> u32 {{ target_fn() }}");
        }
        write_fixture(tmp.path(), "src/callers.rs", &prod);
        let mut tests = String::new();
        for i in 0..65 {
            let _ = writeln!(
                tests,
                "#[test]\nfn t{i}() {{ assert_eq!(target_fn(), 1); }}"
            );
        }
        write_fixture(tmp.path(), "src/cases_test.rs", &tests);
        let bloom = BloomFilterCache::default();
        let session = crate::session::Session::default();
        let result = grok(
            "target_fn",
            tmp.path(),
            &bloom,
            &session,
            GrokCaps::default(),
        )
        .unwrap();
        assert_eq!((result.callers.len(), result.total_callers), (5, 75));
        assert_eq!((result.tests.len(), result.total_tests), (8, 65));
        let text = format_grok(&result, tmp.path());
        assert!(text.contains("## callers (5 of 75)"), "{text}");
        assert!(text.contains("## tests (8 of 65)"), "{text}");
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

    /// A Markdown heading that names the symbol counts as a definition in
    /// search, but it is no rival for grok to pick: only code definitions
    /// make the target ambiguous.
    #[test]
    fn doc_headings_do_not_make_a_target_ambiguous() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(
            tmp.path(),
            "lib.rs",
            "pub fn render_page() -> u32 {\n    1\n}\n",
        );
        write_fixture(
            tmp.path(),
            "README.md",
            "# Docs\n\n## render_page\n\nText.\n",
        );
        write_fixture(
            tmp.path(),
            "GUIDE.md",
            "# More\n\n## render_page\n\nMore.\n",
        );
        let target = grok_target(tmp.path(), "render_page").unwrap();
        assert_eq!(target.other_def_count, 0);
        write_fixture(
            tmp.path(),
            "other.rs",
            "pub fn render_page() -> u32 {\n    2\n}\n",
        );
        let target = grok_target(tmp.path(), "render_page").unwrap();
        assert_eq!(target.other_def_count, 1);
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
        // Missing-file wins over every content judgement — including for
        // non-code extensions, where a typo'd path must not be reported as
        // "not a code file".
        let tmp = tempfile::tempdir().unwrap();
        for missing in ["nope.rs", "nope.md"] {
            let path = tmp.path().join(missing);
            let err = resolve_by_path_line(&path, 1).unwrap_err();
            assert!(
                matches!(err, TilthError::IoError { .. }),
                "{missing}: expected IoError, got {err}"
            );
        }
    }

    #[test]
    fn resolve_by_path_line_refuses_binary_code_file_by_design() {
        // NUL bytes are valid UTF-8 — the old strict read grokked this file
        // happily, so this refusal is a deliberate change, not a bug fix.
        // Strict read_to_string doubled as an accidental DoS guard (garbage
        // is almost never valid UTF-8, so it errored instantly); decoding
        // lossily removes that guard (measured: 6.7s / 1.07GB peak RSS on a
        // 5MB garbage .rs). The binary probe and the size cap replace it
        // with explicit, recoverable refusals.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tainted.rs");
        fs::write(&path, b"fn alpha() {}\n\x00\x01\x02").unwrap();
        let err = resolve_by_path_line(&path, 1).unwrap_err();
        match err {
            TilthError::InvalidQuery { reason, .. } => {
                assert_eq!(
                    reason,
                    "binary or non-text file (null bytes in first 512B) — grok needs source; \
                     use tilth_search \"<symbol>\" for matches in this file"
                );
            }
            other => panic!("expected InvalidQuery, got {other}"),
        }
    }

    #[test]
    fn resolve_by_path_line_reads_non_utf8_code_file_lossily() {
        // Invalid UTF-8, no null byte — the read path shows this as text, so
        // grok must outline it lossily rather than error.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("latin1.rs");
        fs::write(&path, b"fn alpha() {} // caf\xff\n").unwrap();
        let (target, _content, lang) = resolve_by_path_line(&path, 1).unwrap();
        assert_eq!(target.name, "alpha");
        assert_eq!(lang, Lang::Rust);
    }

    #[test]
    fn resolve_by_path_line_refuses_over_cap_code_file() {
        // The binary probe only inspects the first 512B, so a clean head
        // followed by megabytes of garbage slips past it into the lossy
        // decode and the outline parser. The size cap must refuse first.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cleanhead_garbage.rs");
        let mut body = Vec::with_capacity(3_000_512);
        body.extend_from_slice(b"fn cleanhead_alpha() { let x = 1; }\n");
        body.resize(512, b'a');
        let mut state: u32 = 0x9e37_79b9;
        body.extend((0..3_000_000).map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        }));
        fs::write(&path, &body).unwrap();
        let err = resolve_by_path_line(&path, 1).unwrap_err();
        match err {
            TilthError::InvalidQuery { reason, .. } => {
                assert!(
                    reason.contains("too large"),
                    "expected size refusal: {reason}"
                );
                assert!(
                    reason.contains("tilth_search"),
                    "refusal must point at recovery: {reason}"
                );
            }
            other => panic!("expected InvalidQuery, got {other}"),
        }
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

    #[test]
    fn resolve_by_name_refusal_names_the_query_not_the_path() {
        // The refusal fires inside enrich_from_outline after the symbol search
        // already resolved a path; the error must still name what the user
        // asked for, not the file the search walked to.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tainted.rs");
        fs::write(&path, b"fn alpha() {}\n\x00\x01\x02").unwrap();
        let err = resolve_by_name("alpha", tmp.path()).unwrap_err();
        match err {
            TilthError::InvalidQuery { query, .. } => assert_eq!(query, "alpha"),
            other => panic!("expected InvalidQuery, got {other}"),
        }
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
        let session = crate::session::Session::default();
        let result = grok(
            "target_fn",
            tmp.path(),
            &bloom,
            &session,
            GrokCaps::default(),
        )
        .unwrap();

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

    /// Rust calls inside macro arguments are call sites. tree-sitter-rust parses
    /// a macro's arguments as an opaque `token_tree`, so `assert!(target_fn())`
    /// held no `call_expression` and the caller went unreported.
    #[test]
    fn grok_finds_rust_callers_inside_macro_arguments() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(
            tmp.path(),
            "src/lib.rs",
            "pub fn target_fn() -> bool { true }\n",
        );
        let caller = "\
use crate::target_fn;

pub fn caller_fn() {
    assert!(target_fn());
}
";
        write_fixture(tmp.path(), "src/caller.rs", caller);

        let bloom = BloomFilterCache::default();
        let session = crate::session::Session::default();
        let result = grok(
            "target_fn",
            tmp.path(),
            &bloom,
            &session,
            GrokCaps::default(),
        )
        .unwrap();

        let caller_names: Vec<&str> = result
            .callers
            .iter()
            .map(|c| c.calling_function.as_str())
            .collect();
        assert_eq!(caller_names, ["caller_fn"]);
    }

    /// The caller walk quit at 50 call sites, so grok's caller total was
    /// wherever the threads stopped, and the callers shown changed between
    /// runs. 60 files with two callers each: the total is exact, and the five
    /// shown name five files, the same five every run.
    #[test]
    fn grok_caller_totals_are_exact_and_spread_across_files() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(tmp.path(), "src/lib.rs", "pub fn target_fn() {}\n");
        for i in 0..60 {
            write_fixture(
                tmp.path(),
                &format!("src/c{i:02}.rs"),
                &format!("pub fn a{i}() {{ target_fn(); }}\npub fn b{i}() {{ target_fn(); }}\n"),
            );
        }
        let bloom = BloomFilterCache::default();
        let run = || {
            let session = crate::session::Session::default();
            grok(
                "target_fn",
                tmp.path(),
                &bloom,
                &session,
                GrokCaps::default(),
            )
            .unwrap()
        };
        let first = run();
        assert_eq!(first.total_callers, 120);
        let shown = |r: &GrokResult| -> Vec<String> {
            r.callers
                .iter()
                .map(|c| c.calling_function.clone())
                .collect()
        };
        assert_eq!(shown(&first), ["a0", "a1", "a2", "a3", "a4"]);
        for _ in 0..3 {
            let again = run();
            assert_eq!(again.total_callers, 120);
            assert_eq!(shown(&again), shown(&first));
        }
    }

    /// Rust tests live in `tests/` and in `#[test]` functions, not in
    /// `.test.`/`.spec.` files: both are tests, not callers.
    #[test]
    fn grok_counts_rust_test_dirs_and_test_functions_as_tests() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = "\
pub fn target_fn() -> u32 { 1 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_calls_target() {
        let _ = target_fn();
    }
}
";
        write_fixture(tmp.path(), "src/lib.rs", lib);
        let integration = "\
use crate::target_fn;

#[test]
fn integration_calls_target() {
    let _ = target_fn();
}
";
        write_fixture(tmp.path(), "tests/target.rs", integration);

        let bloom = BloomFilterCache::default();
        let session = crate::session::Session::default();
        let result = grok(
            "target_fn",
            tmp.path(),
            &bloom,
            &session,
            GrokCaps::default(),
        )
        .unwrap();

        let mut test_names: Vec<&str> = result.tests.iter().map(|t| t.test_name.as_str()).collect();
        test_names.sort_unstable();
        let caller_names: Vec<&str> = result
            .callers
            .iter()
            .map(|c| c.calling_function.as_str())
            .collect();
        assert_eq!(
            (test_names, caller_names),
            (
                vec!["integration_calls_target", "tests.unit_calls_target"],
                vec![]
            )
        );
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
        let session = crate::session::Session::default();
        let result = grok("target", tmp.path(), &bloom, &session, GrokCaps::default()).unwrap();
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
            delegate_body: None,
            skipped_large: Vec::new(),
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
            delegate_body: None,
            skipped_large: Vec::new(),
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
        let session = crate::session::Session::default();
        let result = grok(
            "target_fn",
            tmp.path(),
            &bloom,
            &session,
            GrokCaps::default(),
        )
        .unwrap();
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
        let session = crate::session::Session::default();
        let result = grok("target", tmp.path(), &bloom, &session, caps).unwrap();
        assert_eq!(result.callees_internal.len(), 3, "callees capped");
        assert_eq!(result.siblings.len(), 2, "siblings capped");
        assert!(
            result.total_callees_internal >= 8,
            "pre-cap total preserved"
        );
        assert!(result.total_siblings >= 8);
    }

    // ── GROK-DEDUP tests ───────────────────────────────────────────

    /// Source with a body big enough to trigger degradation (> `BODY_DEGRADE_THRESHOLD`).
    fn long_body_fixture(name: &str) -> String {
        let mut s = format!("pub fn {name}() {{\n");
        for i in 0..20 {
            let _ = writeln!(s, "    let v{i} = {i};");
        }
        s.push_str("}\n");
        s
    }

    #[test]
    fn grok_repeat_call_degrades_target_body() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(tmp.path(), "src/lib.rs", &long_body_fixture("big"));
        let bloom = BloomFilterCache::default();
        let session = crate::session::Session::default();

        let r1 = grok("big", tmp.path(), &bloom, &session, GrokCaps::default()).unwrap();
        assert!(
            !r1.body.contains("shown earlier"),
            "first call must be full body"
        );

        let r2 = grok("big", tmp.path(), &bloom, &session, GrokCaps::default()).unwrap();
        assert!(
            r2.body.contains("shown earlier"),
            "second call must degrade"
        );
        // Degraded body should still anchor with the signature — first line preserved.
        assert!(r2.body.starts_with("pub fn big()"), "signature preserved");
    }

    /// Session dedup decides "already inlined?" and records the expansion
    /// under two separate lock acquisitions, with the whole callers/callees/
    /// tests pass in between. Two groks of the same target therefore both
    /// observe "not expanded" and both inline the full body — the dedup pays
    /// for itself only if the check and the record are one operation.
    #[test]
    fn concurrent_groks_of_one_target_inline_the_body_once() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(tmp.path(), "src/lib.rs", &long_body_fixture("shared"));
        let bloom = BloomFilterCache::default();
        let session = crate::session::Session::default();
        let barrier = std::sync::Barrier::new(2);

        let (a, b) = std::thread::scope(|s| {
            let ta = s.spawn(|| {
                barrier.wait();
                grok("shared", tmp.path(), &bloom, &session, GrokCaps::default()).unwrap()
            });
            let tb = s.spawn(|| {
                barrier.wait();
                grok("shared", tmp.path(), &bloom, &session, GrokCaps::default()).unwrap()
            });
            (ta.join().unwrap(), tb.join().unwrap())
        });

        let degraded = [&a, &b]
            .iter()
            .filter(|r| r.body.contains("shown earlier"))
            .count();
        assert_eq!(
            degraded, 1,
            "exactly one of two concurrent groks may inline the full body"
        );
    }

    #[test]
    fn grok_short_body_not_degraded() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(tmp.path(), "src/lib.rs", "pub fn tiny() { 1 }\n");
        let bloom = BloomFilterCache::default();
        let session = crate::session::Session::default();

        let r1 = grok("tiny", tmp.path(), &bloom, &session, GrokCaps::default()).unwrap();
        let r2 = grok("tiny", tmp.path(), &bloom, &session, GrokCaps::default()).unwrap();
        assert_eq!(r1.body, r2.body, "sub-threshold body should never degrade");
        assert!(!r2.body.contains("shown earlier"));
    }

    #[test]
    fn grok_stale_after_edit_re_inlines_full_body() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("src/lib.rs");
        write_fixture(tmp.path(), "src/lib.rs", &long_body_fixture("stale"));
        let bloom = BloomFilterCache::default();
        let session = crate::session::Session::default();

        let _ = grok("stale", tmp.path(), &bloom, &session, GrokCaps::default()).unwrap();

        // Bump mtime by overwriting (sleep 10ms to guarantee mtime tick on coarse FS).
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&path, long_body_fixture("stale")).unwrap();

        let r2 = grok("stale", tmp.path(), &bloom, &session, GrokCaps::default()).unwrap();
        assert!(
            !r2.body.contains("shown earlier"),
            "post-edit re-grok must re-inline full body, got: {}",
            r2.body
        );
    }

    // ── GROK-GROUP tests ───────────────────────────────────────────

    #[test]
    fn format_groups_callers_by_file() {
        // The fixture from existing tests already produces 1 caller in 1 file.
        // Verify the rendered output uses the grouped shape (file header + indented sites).
        let tmp = tempfile::tempdir().unwrap();
        let lib = r"
pub fn target() { let _ = 1; }
pub fn caller_a() { target(); }
pub fn caller_b() { target(); }
";
        write_fixture(tmp.path(), "src/lib.rs", lib);
        let bloom = BloomFilterCache::default();
        let session = crate::session::Session::default();
        let result = grok("target", tmp.path(), &bloom, &session, GrokCaps::default()).unwrap();
        let rendered = format_grok(&result, tmp.path());

        // File header appears once, then per-site lines are indented.
        let callers_block = rendered
            .split("## callers")
            .nth(1)
            .expect("## callers section missing");
        assert!(
            callers_block.contains("\n  src/lib.rs\n"),
            "expected file header in callers block: {callers_block}"
        );
        // Per-site lines render as `    [<line>]   in <fn>()`.
        assert!(
            callers_block.contains("in caller_a()"),
            "caller_a should appear: {callers_block}"
        );
        assert!(
            callers_block.contains("in caller_b()"),
            "caller_b should appear: {callers_block}"
        );
    }

    #[test]
    fn format_groups_callees_by_file() {
        // target_fn calls helper (same file) and peer (same file). Verify
        // the single file header collapses both callees under it.
        let tmp = tempfile::tempdir().unwrap();
        let lib = r"
pub fn helper() -> u32 { 1 }
pub fn peer() {}
pub fn target_fn() {
    let _ = helper();
    peer();
}
";
        write_fixture(tmp.path(), "src/lib.rs", lib);
        let bloom = BloomFilterCache::default();
        let session = crate::session::Session::default();
        let result = grok(
            "target_fn",
            tmp.path(),
            &bloom,
            &session,
            GrokCaps::default(),
        )
        .unwrap();
        let rendered = format_grok(&result, tmp.path());

        let callees_block = rendered
            .split("## callees")
            .nth(1)
            .expect("## callees section missing");
        // Both callees live under the same file header.
        let lib_pos = callees_block
            .find("\n  src/lib.rs\n")
            .expect("file header missing");
        let helper_pos = callees_block.find("helper").expect("helper missing");
        let peer_pos = callees_block.find("peer").expect("peer missing");
        assert!(lib_pos < helper_pos, "file header must precede helper");
        assert!(lib_pos < peer_pos, "file header must precede peer");
        // No second `src/lib.rs` header — the grouping collapsed them.
        let later = &callees_block[lib_pos + 1..];
        assert!(
            !later.contains("\n  src/lib.rs\n"),
            "file header should appear once, not duplicated"
        );
    }

    // ── THINWRAP tests ──────────────────────────────────────────────────

    #[test]
    fn read_delegate_content_caps_cross_file_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target.rs");
        let callee = tmp.path().join("callee.rs");
        std::fs::write(&callee, "fn delegate() { real_work() }\n").unwrap();

        // Same file: reuse the passed-in content, no disk read of `target`
        // (which is never written here).
        let same = read_delegate_content(&target, &target, "TARGET BODY", 1000);
        assert_eq!(same.as_deref(), Some("TARGET BODY"));

        // Cross-file under the cap: read the callee file.
        let small = read_delegate_content(&callee, &target, "TARGET BODY", 1000);
        assert!(small.is_some_and(|c| c.contains("real_work")));

        // Cross-file over the cap: skipped (cap=5 < the 30-byte callee). Guards
        // against pulling a huge or generated file into the bundle.
        let capped = read_delegate_content(&callee, &target, "TARGET BODY", 5);
        assert!(capped.is_none(), "oversized callee file must not be read");

        // Missing cross-file: None, not a panic.
        let missing = tmp.path().join("nope.rs");
        assert!(
            read_delegate_content(&missing, &target, "TARGET BODY", 1000).is_none(),
            "missing callee file must yield None, not panic"
        );
    }

    /// A wrapper function: small body (≤10 lines) that calls exactly one
    /// internal function, with no external callees.
    #[test]
    fn thinwrap_wrapper_expands_delegate_body() {
        let tmp = tempfile::tempdir().unwrap();
        // `do_work` is the real implementation with its own distinct logic.
        // `wrapper` just delegates to it — a classic thin wrapper.
        let lib = r"pub fn do_work(x: u32) -> u32 {
    let step1 = x * 2;
    let step2 = step1 + 7;
    step2
}

pub fn wrapper(x: u32) -> u32 {
    do_work(x)
}
";
        write_fixture(tmp.path(), "src/lib.rs", lib);
        let bloom = BloomFilterCache::default();
        let session = crate::session::Session::default();
        let result = grok("wrapper", tmp.path(), &bloom, &session, GrokCaps::default()).unwrap();

        // The delegate body field must be set.
        assert!(
            result.delegate_body.is_some(),
            "thin wrapper must produce delegate_body"
        );

        // The callee's implementation line must be present in the delegate body,
        // not just in the wrapper's own body (which only contains `do_work(x)`).
        let (_, delegate_text) = result.delegate_body.as_ref().unwrap();
        assert!(
            delegate_text.contains("step1 = x * 2"),
            "delegate body must contain callee implementation, got: {delegate_text:?}"
        );

        // The wrapper's own body does NOT contain `step1`.
        assert!(
            !result.body.contains("step1"),
            "wrapper body should not contain callee internals, got: {:?}",
            result.body
        );

        // format_grok must emit a `## delegate body` section between body and callees.
        let rendered = format_grok(&result, tmp.path());
        assert!(
            rendered.contains("## delegate body"),
            "formatted output must contain delegate body heading, got: {rendered}"
        );
        assert!(
            rendered.contains("step1 = x * 2"),
            "formatted output must contain callee implementation line, got: {rendered}"
        );
        // Delegate body section must appear before the callees section.
        let delegate_pos = rendered
            .find("## delegate body")
            .expect("## delegate body missing");
        let callees_pos = rendered.find("## callees").expect("## callees missing");
        assert!(
            delegate_pos < callees_pos,
            "delegate body must appear before callees section"
        );
    }

    /// A non-wrapper function: body > `WRAPPER_MAX_BODY_LINES`. Must NOT expand.
    #[test]
    fn thinwrap_non_wrapper_does_not_expand() {
        let tmp = tempfile::tempdir().unwrap();
        // Substantial function body (> 10 lines).
        let mut lib = String::from("pub fn helper() -> u32 { 42 }\n\npub fn big_fn() {\n");
        for i in 0..12u32 {
            let _ = writeln!(lib, "    let v{i} = {i};");
        }
        lib.push_str("    let _ = helper();\n}\n");
        write_fixture(tmp.path(), "src/lib.rs", &lib);
        let bloom = BloomFilterCache::default();
        let session = crate::session::Session::default();
        let result = grok("big_fn", tmp.path(), &bloom, &session, GrokCaps::default()).unwrap();

        // Body is large — no delegate expansion.
        assert!(
            result.delegate_body.is_none(),
            "large-body function must not produce delegate_body"
        );
        let rendered = format_grok(&result, tmp.path());
        assert!(
            !rendered.contains("## delegate body"),
            "large-body function must not render delegate body section"
        );
    }

    /// A function with multiple callees: must NOT expand even if body is short.
    #[test]
    fn thinwrap_multiple_callees_does_not_expand() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = r"pub fn a() -> u32 { 1 }
pub fn b() -> u32 { 2 }

pub fn multi(x: u32) -> u32 {
    a() + b()
}
";
        write_fixture(tmp.path(), "src/lib.rs", lib);
        let bloom = BloomFilterCache::default();
        let session = crate::session::Session::default();
        let result = grok("multi", tmp.path(), &bloom, &session, GrokCaps::default()).unwrap();

        assert!(
            result.delegate_body.is_none(),
            "multi-callee function must not produce delegate_body"
        );
        let rendered = format_grok(&result, tmp.path());
        assert!(
            !rendered.contains("## delegate body"),
            "multi-callee function must not render delegate body section"
        );
    }

    /// No recursion: a wrapper whose single callee is itself a thin wrapper
    /// must only expand ONE level — the immediate callee, not its callee too.
    #[test]
    fn thinwrap_no_recursion_one_level_only() {
        let tmp = tempfile::tempdir().unwrap();
        // inner_impl is the real work; mid delegates to inner_impl (a wrapper);
        // outer delegates to mid (another wrapper). We grok outer.
        // Only mid's body should appear in the delegate body — not inner_impl's.
        let lib = r"pub fn inner_impl(x: u32) -> u32 {
    let result = x * x + 1;
    result
}

pub fn mid(x: u32) -> u32 {
    inner_impl(x)
}

pub fn outer(x: u32) -> u32 {
    mid(x)
}
";
        write_fixture(tmp.path(), "src/lib.rs", lib);
        let bloom = BloomFilterCache::default();
        let session = crate::session::Session::default();
        let result = grok("outer", tmp.path(), &bloom, &session, GrokCaps::default()).unwrap();

        // Delegate body is set (outer is a thin wrapper around mid).
        assert!(
            result.delegate_body.is_some(),
            "outer is a thin wrapper — must produce delegate_body"
        );

        let (callee, delegate_text) = result.delegate_body.as_ref().unwrap();
        // The immediate callee is `mid`, not `inner_impl`.
        assert_eq!(
            callee.name, "mid",
            "delegate callee should be 'mid', got {:?}",
            callee.name
        );
        // mid's own body contains only the call to inner_impl.
        assert!(
            delegate_text.contains("inner_impl"),
            "mid's body must contain the inner_impl call, got: {delegate_text:?}"
        );
        // inner_impl's internals (`result = x * x + 1`) must NOT appear —
        // that would indicate two levels of expansion.
        assert!(
            !delegate_text.contains("x * x + 1"),
            "two-level expansion must not occur — inner_impl body must not appear, got: {delegate_text:?}"
        );
    }

    /// A large function (definition span > `WRAPPER_MAX_BODY_LINES`) that happens to
    /// call exactly one internal helper must NOT be treated as a wrapper — even on a
    /// re-grok where the body is dedup-degraded to a short preview. Guards against
    /// measuring the degradable body instead of the true definition span.
    #[test]
    fn thinwrap_large_single_call_fn_not_expanded_even_on_regrok() {
        let mut lib = String::from(
            "pub fn helper(x: u32) -> u32 { x + 1 }\n\npub fn biggish(x: u32) -> u32 {\n",
        );
        // 18 trivial bindings push the span past BODY_DEGRADE_THRESHOLD so the body
        // degrades on re-grok; the lone call is the single internal callee.
        for i in 0..18 {
            let _ = writeln!(lib, "    let v{i} = x;");
        }
        lib.push_str("    helper(v17)\n}\n");

        let tmp = tempfile::tempdir().unwrap();
        write_fixture(tmp.path(), "src/lib.rs", &lib);
        let bloom = BloomFilterCache::default();
        let session = crate::session::Session::default();

        let r1 = grok("biggish", tmp.path(), &bloom, &session, GrokCaps::default()).unwrap();
        assert!(
            r1.delegate_body.is_none(),
            "large fn must not expand on first grok"
        );

        // Re-grok: body is dedup-degraded to a short preview, but the definition
        // span is unchanged (> WRAPPER_MAX_BODY_LINES) so it must still not expand.
        let r2 = grok("biggish", tmp.path(), &bloom, &session, GrokCaps::default()).unwrap();
        assert!(
            r2.delegate_body.is_none(),
            "large fn must not expand on re-grok despite degraded body"
        );
    }

    #[test]
    fn type_method_regression_real_impl_block() {
        // Regression for commit 2f7c448 (fix #61): `Type::method` qualified targets
        // must resolve to the method itself, not return NotFound. The `start_line`
        // assertion confirms the resolver lands on the method inside the impl block.
        let tmp = tempfile::tempdir().unwrap();
        let body = "\
pub struct OutlineCache;

pub struct ParsedFile;

impl OutlineCache {
    pub fn new() -> Self {
        Self
    }

    pub fn get_or_parse(&self, _path: &str) -> Option<ParsedFile> {
        None
    }
}
";
        write_fixture(tmp.path(), "src/cache.rs", body);

        for spec in ["OutlineCache::get_or_parse", "OutlineCache.get_or_parse"] {
            let (target, _, _) = resolve_with_source(spec, tmp.path())
                .unwrap_or_else(|e| panic!("`{spec}` must resolve, got: {e}"));
            assert_eq!(
                target.name, "get_or_parse",
                "`{spec}` must resolve to method `get_or_parse`, not the qualified form"
            );
            // get_or_parse is the 10th line of the fixture (new() occupies lines 6-8);
            // assert the exact line so a wrong resolution to `new` or a stub would fail.
            assert_eq!(
                target.start_line, 10,
                "`{spec}` resolved to line {} — expected get_or_parse at line 10",
                target.start_line
            );
            assert_eq!(
                target.other_def_count, 0,
                "exactly one `get_or_parse` definition in the fixture"
            );
        }
    }

    /// Go package-level `type` and grouped `const` declarations resolve by
    /// name, each to its own spec's line, kind and siblings.
    #[test]
    fn grok_resolves_go_type_and_grouped_const() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(
            tmp.path(),
            "main.go",
            tilth_core::lang::go::GO_DECLARATIONS_FIXTURE,
        );

        let bloom = BloomFilterCache::default();
        let session = crate::session::Session::default();
        for (name, kind, line) in [
            ("Server", OutlineKind::Struct, 9),
            ("StatusActive", OutlineKind::Constant, 4),
        ] {
            let result = grok(name, tmp.path(), &bloom, &session, GrokCaps::default())
                .unwrap_or_else(|e| panic!("grok {name} failed: {e}"));
            assert_eq!(result.target.name, name);
            assert_eq!(result.target.kind, kind, "{name} kind");
            assert_eq!(
                (result.target.start_line, result.target.end_line),
                (line, line),
                "{name} range"
            );
            let sibling_names: Vec<&str> =
                result.siblings.iter().map(|s| s.name.as_str()).collect();
            assert!(
                sibling_names.contains(&"NewServer"),
                "{name}: expected NewServer among siblings, got {sibling_names:?}"
            );
        }
    }

    fn assert_test_callers(fixtures: [(&str, &str); 3]) {
        let tmp = tempfile::tempdir().unwrap();
        for (path, source) in fixtures {
            write_fixture(tmp.path(), path, source);
        }
        let bloom = BloomFilterCache::default();
        let session = crate::session::Session::default();
        let result = grok("target", tmp.path(), &bloom, &session, GrokCaps::default()).unwrap();
        assert_eq!(result.total_callers, 1);
        assert_eq!(result.total_tests, 1);
        assert!(result.callers[0].path.ends_with(fixtures[1].0));
        assert!(result.tests[0].path.ends_with(fixtures[2].0));
    }

    #[test]
    fn grok_recognizes_go_test_callers() {
        assert_test_callers([
            ("main.go", "package example\nfunc target() {}\n"),
            (
                "test_support.go",
                "package example\nfunc caller() { target() }\n",
            ),
            (
                "main_test.go",
                "package example\nfunc TestTarget() { target() }\n",
            ),
        ]);
    }

    #[test]
    fn grok_recognizes_python_test_callers() {
        assert_test_callers([
            ("main.py", "def target():\n    pass\n"),
            ("test_support/worker.py", "def caller():\n    target()\n"),
            ("test_main.py", "def test_target():\n    target()\n"),
        ]);
    }
}
