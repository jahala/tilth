use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use crate::types::{is_test_file, Match};

const VENDOR_DIRS: &[&str] = &[
    "node_modules",
    "vendor",
    "dist",
    "build",
    ".git",
    "target",
    "__pycache__",
    ".venv",
    "venv",
    "pkg",
    "out",
];

/// Sort matches by score (highest first). Deterministic: same inputs, same order.
/// When `context` is provided, matches near the context file are boosted.
pub fn sort(matches: &mut [Match], query: &str, scope: &Path, context: Option<&Path>) {
    // Pre-compute context's package root once (same for entire batch)
    let ctx_parent = context.and_then(|c| c.parent());
    let ctx_pkg_root = context
        .and_then(package_root)
        .map(std::path::Path::to_path_buf);

    // Cache package roots for match paths — avoids repeated stat walks
    let mut pkg_cache: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
    // Capture now once so the sort comparator does not call SystemTime::now() O(n log n) times.
    let now = SystemTime::now();

    matches.sort_by(|a, b| {
        let sa = score(
            a,
            query,
            scope,
            ctx_parent,
            ctx_pkg_root.as_ref(),
            &mut pkg_cache,
            now,
        );
        let sb = score(
            b,
            query,
            scope,
            ctx_parent,
            ctx_pkg_root.as_ref(),
            &mut pkg_cache,
            now,
        );
        sb.cmp(&sa)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
}

/// Ranking function. Each match gets a score — no floating point, no randomness.
/// All boosts are positive (added), all penalties are positive (subtracted).
fn score(
    m: &Match,
    query: &str,
    scope: &Path,
    ctx_parent: Option<&Path>,
    ctx_pkg_root: Option<&PathBuf>,
    pkg_cache: &mut HashMap<PathBuf, Option<PathBuf>>,
    now: SystemTime,
) -> i32 {
    let mut s = 0i32;

    if m.is_definition {
        s += i32::from(m.def_weight) * 10;
        s += definition_name_boost(m, query);
    }
    if m.exact {
        s += 500;
    }

    s += query_intent_boost(m, query);
    s += multi_word_boost(m, query);
    s += scope_proximity(&m.path, scope) as i32;
    s += recency(m.mtime, now) as i32;

    if m.file_lines > 0 && m.file_lines < 200 {
        s += 50;
    }

    // Context-aware boosts
    if ctx_parent.is_some() || ctx_pkg_root.is_some() {
        s += context_proximity(&m.path, ctx_parent, ctx_pkg_root, pkg_cache);
    }

    s += basename_boost(&m.path, query);
    s += exported_api_boost(m);
    s -= non_code_penalty(&m.path);
    s -= incidental_text_penalty(m, query);

    if is_test_file(&m.path) && !looks_like_test_query(query) {
        s -= 120;
    }
    s -= fixture_penalty(m);

    // Vendor penalty (always active)
    if is_vendor_path(&m.path) {
        s -= 200;
    }

    s
}

/// Boost matches whose file stem matches the query.
fn basename_boost(path: &Path, query: &str) -> i32 {
    if query.is_empty() {
        return 0;
    }

    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return 0;
    };
    let stem_lower = stem.to_ascii_lowercase();
    let query_lower = query.to_ascii_lowercase();

    if stem_lower == query_lower {
        return 500;
    }
    if stem_lower.starts_with(&query_lower)
        && stem_lower
            .as_bytes()
            .get(query_lower.len())
            .is_some_and(|&b| b == b'_' || b == b'.' || b == b'-')
    {
        return 350;
    }
    if stem_lower.ends_with(&query_lower) {
        return 250;
    }
    if stem_lower.contains(&query_lower) {
        return 180;
    }

    let parent_name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if parent_name.eq_ignore_ascii_case(query) {
        return 200;
    }

    0
}

/// 0-200, closer to scope root = higher.
fn scope_proximity(path: &Path, scope: &Path) -> u32 {
    let rel = path.strip_prefix(scope).unwrap_or(path);
    let depth = rel.components().count();
    200u32.saturating_sub(depth as u32 * 20)
}

/// Context-aware proximity boost with cached package roots.
fn context_proximity(
    match_path: &Path,
    ctx_parent: Option<&Path>,
    ctx_pkg_root: Option<&PathBuf>,
    pkg_cache: &mut HashMap<PathBuf, Option<PathBuf>>,
) -> i32 {
    let mut score = 0;

    // Same directory as context file
    if let Some(cp) = ctx_parent {
        if match_path.parent() == Some(cp) {
            score += 100;
        } else if shared_prefix_depth(cp, match_path.parent().unwrap_or(match_path)) >= 2 {
            score += 40;
        }
    }

    // Same package root (cached)
    if let Some(cp_root) = ctx_pkg_root {
        let match_dir = match match_path.parent() {
            Some(d) => d.to_path_buf(),
            None => return score,
        };
        let match_root = pkg_cache
            .entry(match_dir)
            .or_insert_with_key(|dir| package_root(dir).map(std::path::Path::to_path_buf));
        if let Some(ref mr) = match_root {
            if mr == cp_root {
                score += 75;
            }
        }
    }

    score
}

fn definition_name_boost(m: &Match, query: &str) -> i32 {
    let Some(name) = m.def_name.as_deref() else {
        return 0;
    };

    let query_lower = query.to_ascii_lowercase();
    let name_lower = name.to_ascii_lowercase();

    if name == query {
        220
    } else if name_lower == query_lower {
        180
    } else if m.impl_target.as_deref() == Some(query) {
        120
    } else if name_lower.starts_with(&query_lower) {
        80
    } else if name_lower.contains(&query_lower) {
        40
    } else {
        0
    }
}

fn query_intent_boost(m: &Match, query: &str) -> i32 {
    if query.is_empty() {
        return 0;
    }

    let looks_type = query.chars().next().is_some_and(char::is_uppercase);
    let looks_fn = query.chars().next().is_some_and(char::is_lowercase);
    let text = m.text.trim_start();

    if looks_type
        && (text.starts_with("struct ")
            || text.starts_with("pub struct ")
            || text.starts_with("enum ")
            || text.starts_with("pub enum ")
            || text.starts_with("trait ")
            || text.starts_with("pub trait ")
            || text.starts_with("interface ")
            || text.starts_with("export interface ")
            || text.starts_with("type ")
            || text.starts_with("export type ")
            || text.starts_with("class ")
            || text.starts_with("export class ")
            || text.starts_with("impl "))
    {
        return 90;
    }

    if looks_fn
        && (text.starts_with("fn ")
            || text.starts_with("pub fn ")
            || text.starts_with("pub(crate) fn ")
            || text.starts_with("async fn ")
            || text.starts_with("pub async fn ")
            || text.starts_with("function ")
            || text.starts_with("export function ")
            || text.starts_with("export default function ")
            || text.starts_with("export async function "))
    {
        return 70;
    }

    0
}

fn exported_api_boost(m: &Match) -> i32 {
    let text = m.text.trim_start();

    if text.starts_with("export default ") {
        90
    } else if text.starts_with("export ") {
        75
    } else if text.starts_with("pub ") {
        60
    } else {
        0
    }
}

/// Penalize matches in test fixtures, mocks, stubs, etc. Capped at 200.
fn fixture_penalty(m: &Match) -> i32 {
    let path = m.path.to_string_lossy().to_ascii_lowercase();
    let text = m.text.to_ascii_lowercase();

    let mut score = 0;
    for needle in ["mock", "fixture", "stub", "fake", "example"] {
        if path.contains(needle) {
            score += 90;
        }
        if text.contains(needle) {
            score += 40;
        }
    }
    score.min(200)
}

/// Penalize matches that appear only in comments (not code).
fn incidental_text_penalty(m: &Match, query: &str) -> i32 {
    if m.is_definition {
        return 0;
    }

    let text = m.text.trim();
    let q_lower = query.to_ascii_lowercase();

    // Only use unambiguous comment prefixes — avoid '#' (Python/C preprocessor/Rust attrs)
    // and '*' (could be pointer deref, multiplication, glob, etc.)
    // Exempt /// doc comments — they're often the most useful context for a symbol.
    let is_comment = (text.starts_with("//") && !text.starts_with("///"))
        || text.starts_with("/*")
        || text.starts_with("<!--");

    // For '#' lines: only treat as comment in languages where # is always a comment
    let is_hash_comment = text.starts_with('#') && {
        let ext = m.path.extension().and_then(|e| e.to_str()).unwrap_or("");
        matches!(
            ext,
            "py" | "rb" | "sh" | "bash" | "zsh" | "yaml" | "yml" | "toml" | "pl" | "r" | "R"
        )
    };

    if is_comment || is_hash_comment {
        return 150;
    }

    // Check if query only appears in a trailing comment (after //)
    // Skip false positives: :// is a URL scheme separator, not a comment
    // Skip // at start of line — that's a full-line comment, not trailing
    let t_lower = text.to_ascii_lowercase();
    if let Some(slash_pos) = t_lower.find("//") {
        let is_url = slash_pos > 0 && t_lower.as_bytes()[slash_pos - 1] == b':';
        if slash_pos > 0
            && !is_url
            && t_lower[slash_pos..].contains(&q_lower)
            && !t_lower[..slash_pos].contains(&q_lower)
        {
            return 100;
        }
    }

    0
}

fn multi_word_boost(m: &Match, query: &str) -> i32 {
    if !query.contains(' ') {
        return 0;
    }

    let words: Vec<&str> = query.split_whitespace().collect();
    if words.len() < 2 {
        return 0;
    }

    let path_lower = m.path.to_string_lossy().to_ascii_lowercase();
    let text_lower = m.text.to_ascii_lowercase();
    let haystack = format!("{path_lower} {text_lower}");

    let matched = words
        .iter()
        .filter(|w| haystack.contains(&w.to_ascii_lowercase()))
        .count();

    if matched == words.len() {
        300
    } else if matched >= words.len() - 1 {
        120
    } else {
        0
    }
}

/// Penalize non-code files: docs, config examples, generated output.
/// Returns positive value (subtracted from score by caller).
/// Note: `dist/`, `build/` are NOT penalized here — they are already covered by `VENDOR_DIRS`.
fn non_code_penalty(path: &Path) -> i32 {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    // Match on path components to avoid false positives (redoc/, javadoc/, pydoc/)
    let has_docs_component = path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| s == "docs" || s == "doc")
    });

    let is_docs = ext == "md" || ext == "mdx" || ext == "txt" || ext == "rst" || has_docs_component;

    let path_str = path.to_string_lossy();
    let is_config_example = (path_str.contains("example")
        || path_str.contains("sample")
        || path_str.contains("template"))
        && (ext == "md" || ext == "txt" || ext == "rst");

    let is_generated = path_str.contains("generated");

    let mut penalty = 0;
    if is_docs {
        penalty += 250;
    }
    if is_config_example {
        penalty += 80;
    }
    if is_generated {
        penalty += 150;
    }
    penalty
}

fn looks_like_test_query(query: &str) -> bool {
    let q = query.to_ascii_lowercase();
    q.contains("test") || q.contains("spec") || q.starts_with("should")
}

fn shared_prefix_depth(a: &Path, b: &Path) -> usize {
    a.components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .zip(b.components().filter(|c| matches!(c, Component::Normal(_))))
        .take_while(|(l, r)| l == r)
        .count()
}

/// Re-export from lang module to keep rank.rs self-contained.
fn package_root(path: &Path) -> Option<&Path> {
    crate::lang::package_root(path)
}

/// Check if path contains a vendor directory component.
fn is_vendor_path(path: &Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| VENDOR_DIRS.contains(&s))
    })
}

/// 0-100, newer = higher. Files modified within the last hour get max score.
fn recency(mtime: SystemTime, now: SystemTime) -> u32 {
    let age = now.duration_since(mtime).unwrap_or_default().as_secs();

    match age {
        0..=3_600 => 100,          // last hour
        3_601..=86_400 => 80,      // last day
        86_401..=604_800 => 50,    // last week
        604_801..=2_592_000 => 20, // last month
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::sort;
    use crate::types::Match;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn make_match(path: &str, text: &str, is_definition: bool, def_name: Option<&str>) -> Match {
        Match {
            path: PathBuf::from(path),
            line: 1,
            text: text.to_string(),
            is_definition,
            exact: true,
            file_lines: 40,
            mtime: SystemTime::now(),
            def_range: None,
            def_name: def_name.map(ToString::to_string),
            def_weight: if is_definition { 80 } else { 0 },
            impl_target: None,
        }
    }

    #[test]
    fn prefers_exact_definition_name_over_usage() {
        let scope = PathBuf::from("/repo/src");
        let mut matches = vec![
            make_match("/repo/src/auth.rs", "handleAuth(user)", false, None),
            make_match(
                "/repo/src/auth.rs",
                "pub fn handleAuth(req: Request) -> Response {",
                true,
                Some("handleAuth"),
            ),
        ];

        sort(&mut matches, "handleAuth", &scope, None);

        assert!(matches[0].is_definition);
        assert_eq!(matches[0].def_name.as_deref(), Some("handleAuth"));
    }

    #[test]
    fn prefers_non_test_match_for_non_test_query() {
        let scope = PathBuf::from("/repo/src");
        let mut matches = vec![
            make_match(
                "/repo/src/__tests__/auth.test.ts",
                "export function handleAuth() {",
                true,
                Some("handleAuth"),
            ),
            make_match(
                "/repo/src/auth.ts",
                "export function handleAuth() {",
                true,
                Some("handleAuth"),
            ),
        ];

        sort(&mut matches, "handleAuth", &scope, None);

        assert_eq!(matches[0].path, PathBuf::from("/repo/src/auth.ts"));
    }

    #[test]
    fn prefers_same_subtree_as_context() {
        let scope = PathBuf::from("/repo/src");
        let context = PathBuf::from("/repo/src/auth/controller.rs");
        let mut matches = vec![
            make_match(
                "/repo/src/payments/service.rs",
                "pub fn handleAuth() {",
                true,
                Some("handleAuth"),
            ),
            make_match(
                "/repo/src/auth/service.rs",
                "pub fn handleAuth() {",
                true,
                Some("handleAuth"),
            ),
        ];

        sort(&mut matches, "handleAuth", &scope, Some(&context));

        assert_eq!(matches[0].path, PathBuf::from("/repo/src/auth/service.rs"));
    }

    #[test]
    fn prefers_exported_api_over_local_definition() {
        let scope = PathBuf::from("/repo/src");
        let mut matches = vec![
            make_match(
                "/repo/src/internal/auth.ts",
                "function handleAuth() {",
                true,
                Some("handleAuth"),
            ),
            make_match(
                "/repo/src/public/auth.ts",
                "export function handleAuth() {",
                true,
                Some("handleAuth"),
            ),
        ];

        sort(&mut matches, "handleAuth", &scope, None);

        assert_eq!(matches[0].path, PathBuf::from("/repo/src/public/auth.ts"));
    }

    #[test]
    fn prefers_real_definition_over_fixture_match() {
        let scope = PathBuf::from("/repo/src");
        let mut matches = vec![
            make_match(
                "/repo/src/fixtures/auth-fixture.ts",
                "export function handleAuth() {",
                true,
                Some("handleAuth"),
            ),
            make_match(
                "/repo/src/auth.ts",
                "export function handleAuth() {",
                true,
                Some("handleAuth"),
            ),
        ];

        sort(&mut matches, "handleAuth", &scope, None);

        assert_eq!(matches[0].path, PathBuf::from("/repo/src/auth.ts"));
    }

    #[test]
    fn prefers_thinking_logic_over_schema_for_concept_query() {
        let scope = PathBuf::from("/repo/src");
        let mut matches = vec![
            make_match(
                "/repo/src/internal/interfaces/client_models.go",
                "ThinkingConfig *GenerationConfigThinkingConfig `json:\"thinkingConfig,omitempty\"`",
                false,
                None,
            ),
            make_match(
                "/repo/src/internal/util/thinking.go",
                "func NormalizeThinkingBudget(model string, requested int) int {",
                true,
                Some("NormalizeThinkingBudget"),
            ),
        ];

        sort(&mut matches, "thinking", &scope, None);

        assert!(
            matches[0].path.to_string_lossy().contains("thinking.go"),
            "expected thinking.go first, got {:?}",
            matches[0].path,
        );
    }

    #[test]
    fn prefers_model_mapping_logic_over_docs_for_alias_query() {
        let scope = PathBuf::from("/repo/src");
        let mut matches = vec![
            make_match(
                "/repo/src/docs/FORCE_HANDLER_GUIDE.md",
                "Alias routing example",
                false,
                None,
            ),
            make_match(
                "/repo/src/internal/api/modules/amp/model_mapping.go",
                "func (m *DefaultModelMapper) MapModel(requestedModel string) string {",
                true,
                Some("MapModel"),
            ),
        ];

        sort(&mut matches, "alias", &scope, None);

        assert!(
            matches[0].path.to_string_lossy().contains("model_mapping"),
            "expected model_mapping.go first, got {:?}",
            matches[0].path,
        );
    }

    // --- Unit tests for individual penalty/boost functions ---

    #[test]
    fn non_code_penalty_docs_positive() {
        // Docs get penalized (positive return value, subtracted by caller)
        let path = PathBuf::from("/repo/docs/guide.md");
        assert!(super::non_code_penalty(&path) > 0);
    }

    #[test]
    fn non_code_penalty_no_double_penalty_for_dist() {
        // dist/ should NOT be penalized here — VENDOR_DIRS handles it
        let path = PathBuf::from("/repo/dist/bundle.js");
        assert_eq!(super::non_code_penalty(&path), 0);
    }

    #[test]
    fn non_code_penalty_no_double_penalty_for_build() {
        let path = PathBuf::from("/repo/build/output.js");
        assert_eq!(super::non_code_penalty(&path), 0);
    }

    #[test]
    fn non_code_penalty_generated_without_dist() {
        let path = PathBuf::from("/repo/src/generated/types.ts");
        assert!(super::non_code_penalty(&path) > 0);
    }

    #[test]
    fn non_code_penalty_normal_code_zero() {
        let path = PathBuf::from("/repo/src/auth.rs");
        assert_eq!(super::non_code_penalty(&path), 0);
    }

    #[test]
    fn fixture_penalty_capped_at_200() {
        // A path hitting multiple needles should be capped
        let m = make_match(
            "/repo/src/fixtures/mock_stub_fake.ts",
            "example fixture mock stub fake",
            false,
            None,
        );
        let penalty = super::fixture_penalty(&m);
        assert!(
            penalty <= 200,
            "fixture_penalty was {penalty}, expected <= 200"
        );
        assert!(penalty > 0);
    }

    #[test]
    fn fixture_penalty_zero_for_normal_code() {
        let m = make_match(
            "/repo/src/auth.ts",
            "export function handleAuth() {",
            true,
            Some("handleAuth"),
        );
        assert_eq!(super::fixture_penalty(&m), 0);
    }

    #[test]
    fn incidental_text_penalty_comment_line() {
        // Lines starting with // should be penalized
        let m = make_match(
            "/repo/src/lib.rs",
            "// handleAuth is deprecated",
            false,
            None,
        );
        assert_eq!(super::incidental_text_penalty(&m, "handleAuth"), 150);
    }

    #[test]
    fn incidental_text_penalty_no_hash_false_positive() {
        // # in C/Rust files should NOT trigger comment penalty
        let m = make_match("/repo/src/main.c", "#include <stdio.h>", false, None);
        assert_eq!(super::incidental_text_penalty(&m, "stdio"), 0);
    }

    #[test]
    fn incidental_text_penalty_hash_comment_in_python() {
        // # in .py files IS a comment — should be penalized
        let m = make_match(
            "/repo/src/main.py",
            "# handle_auth is deprecated",
            false,
            None,
        );
        assert_eq!(super::incidental_text_penalty(&m, "handle_auth"), 150);
    }

    #[test]
    fn incidental_text_penalty_no_star_false_positive() {
        // * should NOT trigger comment penalty
        let m = make_match("/repo/src/main.c", "*ptr = value;", false, None);
        assert_eq!(super::incidental_text_penalty(&m, "ptr"), 0);
    }

    #[test]
    fn incidental_text_penalty_no_string_literal_heuristic() {
        // String literals should NOT be penalized (fragile heuristic removed)
        let m = make_match(
            "/repo/src/lib.rs",
            r#"let msg = "handleAuth error";"#,
            false,
            None,
        );
        assert_eq!(super::incidental_text_penalty(&m, "handleAuth"), 0);
    }

    #[test]
    fn incidental_text_penalty_trailing_comment() {
        // Query only in trailing comment should be penalized
        let m = make_match(
            "/repo/src/lib.rs",
            "let x = 1; // handleAuth workaround",
            false,
            None,
        );
        assert_eq!(super::incidental_text_penalty(&m, "handleAuth"), 100);
    }

    #[test]
    fn incidental_text_penalty_url_not_comment() {
        // :// is a URL scheme — should NOT be treated as trailing comment
        let m = make_match(
            "/repo/src/lib.rs",
            r#"let url = "https://handleAuth.example.com";"#,
            false,
            None,
        );
        assert_eq!(super::incidental_text_penalty(&m, "handleAuth"), 0);
    }

    #[test]
    fn incidental_text_penalty_skip_definitions() {
        // Definitions should never be penalized
        let m = make_match(
            "/repo/src/lib.rs",
            "// handleAuth docs",
            true,
            Some("handleAuth"),
        );
        assert_eq!(super::incidental_text_penalty(&m, "handleAuth"), 0);
    }

    #[test]
    fn incidental_text_penalty_doc_comment_exempt() {
        // /// doc comments should NOT be penalized — they provide useful symbol context
        let m = make_match(
            "/repo/src/lib.rs",
            "/// Handles auth validation for incoming requests",
            false,
            None,
        );
        assert_eq!(super::incidental_text_penalty(&m, "auth"), 0);
    }

    #[test]
    fn sign_convention_all_penalties_positive() {
        // All penalty functions should return >= 0 (positive values, subtracted by score())
        let doc_path = PathBuf::from("/repo/docs/guide.md");
        assert!(super::non_code_penalty(&doc_path) >= 0);

        let fixture = make_match("/repo/fixtures/mock.ts", "mock data", false, None);
        assert!(super::fixture_penalty(&fixture) >= 0);

        let comment = make_match("/repo/src/lib.rs", "// TODO fix auth", false, None);
        assert!(super::incidental_text_penalty(&comment, "auth") >= 0);
    }

    #[test]
    fn vendor_path_detects_dist_and_build() {
        // dist/ and build/ are in VENDOR_DIRS — this is where the penalty comes from
        assert!(super::is_vendor_path(&PathBuf::from(
            "/repo/dist/bundle.js"
        )));
        assert!(super::is_vendor_path(&PathBuf::from(
            "/repo/build/output.js"
        )));
        assert!(!super::is_vendor_path(&PathBuf::from("/repo/src/auth.rs")));
    }
}
