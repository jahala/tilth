//! `tilth_list` — directory tree with per-directory token-cost rollups.
//!
//! Resolves each glob against a walk of `scope`, collects `(path, byte_len)`
//! pairs, and renders them as a single tree rooted at scope.

use std::path::PathBuf;

use serde_json::Value;

use super::{apply_budget, resolve_scope};

pub(in crate::mcp) fn tool_list(args: &Value) -> Result<String, String> {
    use globset::Glob;
    let root = args
        .get("root")
        .and_then(|v| v.as_str())
        .map(std::path::Path::new);
    let (scope, scope_warning) = resolve_scope(args, root)?;
    let budget = args.get("budget").and_then(serde_json::Value::as_u64);

    let patterns_arr = args
        .get("patterns")
        .and_then(|v| v.as_array())
        .ok_or("missing required parameter: patterns (array of globs)")?;
    if patterns_arr.is_empty() {
        return Err("patterns must contain at least one glob".into());
    }
    if patterns_arr.len() > 20 {
        return Err(format!(
            "patterns limited to 20 per call (got {})",
            patterns_arr.len()
        ));
    }
    let patterns: Vec<String> = patterns_arr
        .iter()
        .map(|v| {
            v.as_str()
                .ok_or("patterns must be an array of strings")
                .map(String::from)
        })
        .collect::<Result<_, _>>()?;

    let depth = args
        .get("depth")
        .and_then(serde_json::Value::as_u64)
        .map(|d| d as usize);

    let matchers: Vec<_> = patterns
        .iter()
        .filter_map(|p| Glob::new(p).ok().map(|g| g.compile_matcher()))
        .collect();
    if matchers.is_empty() {
        return Err("no valid globs provided".into());
    }

    // Walk the scope directory (shared junk-dir policy) and collect every file
    // matching any pattern, then render as one token-rolled-up tree.
    let mut entries: Vec<(PathBuf, u64)> = Vec::new();
    let walker = crate::search::base_walk_builder(&scope).build();
    for entry in walker.filter_map(Result::ok) {
        let Some(ft) = entry.file_type() else {
            continue;
        };
        if !ft.is_file() {
            continue;
        }
        let path = entry.path();
        let rel = path.strip_prefix(&scope).unwrap_or(path);
        if let Some(d) = depth {
            if rel.components().count() > d {
                continue;
            }
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let matched = matchers.iter().any(|m| m.is_match(name) || m.is_match(rel));
        if matched {
            let bytes = entry.metadata().map_or(0, |m| m.len());
            entries.push((path.to_path_buf(), bytes));
        }
    }

    let tree = crate::mcp::tree::render_tree(&scope, &entries);
    let mut result = scope_warning.unwrap_or_default();
    result.push_str(&apply_budget(&tree, budget));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a small scratch project with nested .rs and a .toml, returning the
    /// tempdir guard so the caller controls cleanup.
    fn scratch_project() -> tempfile::TempDir {
        let project = tempfile::tempdir().unwrap();
        let p = project.path();
        std::fs::write(p.join("Cargo.toml"), "[package]\nname = \"t\"").unwrap();
        std::fs::create_dir(p.join("src")).unwrap();
        std::fs::write(p.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(p.join("src/lib.rs"), "pub fn x() {}").unwrap();
        project
    }

    #[test]
    fn tool_list_renders_tree_with_dirs_files_and_rollups() {
        let project = scratch_project();
        let args = serde_json::json!({
            "patterns": ["*.rs"],
            "scope": project.path().to_str().unwrap(),
        });
        let out = tool_list(&args).expect("tool_list should succeed");
        // Tree groups the src/ directory and its two .rs leaves.
        assert!(out.contains("src/"), "expected src/ dir node: {out}");
        assert!(out.contains("main.rs"), "expected main.rs leaf: {out}");
        assert!(out.contains("lib.rs"), "expected lib.rs leaf: {out}");
        // *.rs must not pull in the Cargo.toml sibling.
        assert!(
            !out.contains("Cargo.toml"),
            "*.rs must not match Cargo.toml: {out}"
        );
        // Per-node token rollups are the whole point of the tree view.
        assert!(out.contains("tokens"), "expected token rollups: {out}");
    }

    #[test]
    fn tool_list_empty_patterns_errors() {
        let args = serde_json::json!({ "patterns": [], "scope": env!("CARGO_MANIFEST_DIR") });
        let err = tool_list(&args).expect_err("expected empty-patterns error");
        assert!(err.contains("at least one"), "unexpected error: {err}");
    }

    #[test]
    fn tool_list_missing_patterns_errors() {
        let args = serde_json::json!({ "scope": env!("CARGO_MANIFEST_DIR") });
        let err = tool_list(&args).expect_err("expected missing-patterns error");
        assert!(
            err.contains("missing required parameter"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn tool_list_patterns_capped_at_20() {
        let twenty_one: Vec<&str> = vec!["*.rs"; 21];
        let args =
            serde_json::json!({ "patterns": twenty_one, "scope": env!("CARGO_MANIFEST_DIR") });
        let err = tool_list(&args).expect_err("expected cap error");
        assert!(err.contains("limited to 20"), "unexpected error: {err}");
    }

    #[test]
    fn tool_list_depth_caps_nesting() {
        let project = scratch_project();
        // depth=1 keeps only top-level entries; src/*.rs is at depth 2 and drops.
        let args = serde_json::json!({
            "patterns": ["*.toml"],
            "depth": 1,
            "scope": project.path().to_str().unwrap(),
        });
        let out = tool_list(&args).expect("tool_list should succeed");
        assert!(out.contains("Cargo.toml"), "top-level toml kept: {out}");
    }

    #[test]
    fn tool_list_relative_scope_absolute_root_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("a.rs"), "fn a() {}\n").unwrap();
        let args = serde_json::json!({
            "patterns": ["*.rs"],
            "scope": "sub",
            "root": tmp.path().to_str().unwrap(),
        });
        let out = tool_list(&args).expect("relative scope + absolute root resolves");
        assert!(
            out.contains("a.rs"),
            "expected listing under anchored root: {out}"
        );
    }

    #[test]
    fn tool_list_explicit_relative_scope_no_root_errors() {
        let args = serde_json::json!({ "patterns": ["*.rs"], "scope": "some/relative/dir" });
        let err = tool_list(&args).expect_err("explicit relative scope must refuse without root");
        assert!(
            err.contains("relative scope") && err.contains("root"),
            "explicit relative scope without root must refuse: {err}"
        );
    }
}
