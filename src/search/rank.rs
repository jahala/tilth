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
    let ctx_parent = context.and_then(|c| c.parent());
    let ctx_pkg_root = context
        .and_then(package_root)
        .map(std::path::Path::to_path_buf);

    let mut pkg_cache: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
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
    s += scope_proximity(&m.path, scope) as i32;
    s += recency(m.mtime, now) as i32;

    if m.file_lines > 0 && m.file_lines < 200 {
        s += 50;
    }

    if ctx_parent.is_some() || ctx_pkg_root.is_some() {
        s += context_proximity(&m.path, ctx_parent, ctx_pkg_root, pkg_cache);
    }

    s += basename_boost(&m.path, query);

    if is_test_file(&m.path) && !looks_like_test_query(query) {
        s -= 120;
    }

    if is_vendor_path(&m.path) {
        s -= 200;
    }

    s
}

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
        return 300;
    }
    if stem_lower.starts_with(&query_lower)
        && stem_lower
            .as_bytes()
            .get(query_lower.len())
            .is_some_and(|&b| b == b'_' || b == b'.')
    {
        return 150;
    }
    if stem_lower.contains(&query_lower) {
        return 100;
    }
    0
}

fn scope_proximity(path: &Path, scope: &Path) -> u32 {
    let rel = path.strip_prefix(scope).unwrap_or(path);
    let depth = rel.components().count();
    200u32.saturating_sub(depth as u32 * 20)
}

fn context_proximity(
    match_path: &Path,
    ctx_parent: Option<&Path>,
    ctx_pkg_root: Option<&PathBuf>,
    pkg_cache: &mut HashMap<PathBuf, Option<PathBuf>>,
) -> i32 {
    let mut score = 0;

    if let Some(cp) = ctx_parent {
        if match_path.parent() == Some(cp) {
            score += 100;
        } else if shared_prefix_depth(cp, match_path.parent().unwrap_or(match_path)) >= 2 {
            score += 40;
        }
    }

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

    if looks_type {
        if text.starts_with("struct ")
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
            || text.starts_with("impl ")
        {
            return 90;
        }
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

fn looks_like_test_query(query: &str) -> bool {
    let q = query.to_ascii_lowercase();
    q.contains("test") || q.contains("spec") || q.starts_with("should")
}

fn shared_prefix_depth(a: &Path, b: &Path) -> usize {
    a.components()
        .zip(b.components())
        .take_while(|(left, right)| match (left, right) {
            (Component::Normal(l), Component::Normal(r)) => l == r,
            _ => false,
        })
        .count()
}

fn package_root(path: &Path) -> Option<&Path> {
    super::package_root(path)
}

fn is_vendor_path(path: &Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| VENDOR_DIRS.contains(&s))
    })
}

fn recency(mtime: SystemTime, now: SystemTime) -> u32 {
    let age = now.duration_since(mtime).unwrap_or_default().as_secs();

    match age {
        0..=3_600 => 100,
        3_601..=86_400 => 80,
        86_401..=604_800 => 50,
        604_801..=2_592_000 => 20,
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
}
