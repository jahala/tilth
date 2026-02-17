use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::types::Match;

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

    matches.sort_by(|a, b| {
        let sa = score(
            a,
            query,
            scope,
            ctx_parent,
            ctx_pkg_root.as_ref(),
            &mut pkg_cache,
        );
        let sb = score(
            b,
            query,
            scope,
            ctx_parent,
            ctx_pkg_root.as_ref(),
            &mut pkg_cache,
        );
        sb.cmp(&sa)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
}

/// Ranking function. Each match gets a score — no floating point, no randomness.
fn score(
    m: &Match,
    query: &str,
    scope: &Path,
    ctx_parent: Option<&Path>,
    ctx_pkg_root: Option<&PathBuf>,
    pkg_cache: &mut HashMap<PathBuf, Option<PathBuf>>,
) -> i32 {
    let mut s = 0i32;

    if m.is_definition {
        s += i32::from(m.def_weight) * 10;
    }
    if m.exact {
        s += 500;
    }

    s += scope_proximity(&m.path, scope) as i32;
    s += recency(m.mtime) as i32;

    if m.file_lines > 0 && m.file_lines < 200 {
        s += 50;
    }

    // Context-aware boosts
    if ctx_parent.is_some() || ctx_pkg_root.is_some() {
        s += context_proximity(&m.path, ctx_parent, ctx_pkg_root, pkg_cache);
    }

    s += basename_boost(&m.path, query);

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
        return 300; // walk.rs for "walk"
    }
    if stem_lower.starts_with(&query_lower)
        && stem_lower
            .as_bytes()
            .get(query_lower.len())
            .is_some_and(|&b| b == b'_' || b == b'.')
    {
        return 150; // walk_test.rs for "walk"
    }
    if stem_lower.contains(&query_lower) {
        return 100; // tree_walk.rs for "walk"
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
    // Same directory as context file
    if let Some(cp) = ctx_parent {
        if match_path.parent() == Some(cp) {
            return 100;
        }
    }

    // Same package root (cached)
    if let Some(cp_root) = ctx_pkg_root {
        let match_dir = match match_path.parent() {
            Some(d) => d.to_path_buf(),
            None => return 0,
        };
        let match_root = pkg_cache
            .entry(match_dir)
            .or_insert_with_key(|dir| package_root(dir).map(std::path::Path::to_path_buf));
        if let Some(ref mr) = match_root {
            if mr == cp_root {
                return 75;
            }
        }
    }

    0
}

/// Re-export from parent module to keep rank.rs self-contained.
fn package_root(path: &Path) -> Option<&Path> {
    super::package_root(path)
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
fn recency(mtime: SystemTime) -> u32 {
    let age = SystemTime::now()
        .duration_since(mtime)
        .unwrap_or_default()
        .as_secs();

    match age {
        0..=3_600 => 100,          // last hour
        3_601..=86_400 => 80,      // last day
        86_401..=604_800 => 50,    // last week
        604_801..=2_592_000 => 20, // last month
        _ => 0,
    }
}
