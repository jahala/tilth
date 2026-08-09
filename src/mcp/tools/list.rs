//! `tilth_list` — tree output with token-cost rollups.
//!
//! Resolves each glob via `ignore::WalkBuilder`, collects `(path, byte_len)`
//! pairs, and renders them as a single tree rooted at scope.

use std::fmt::Write as _;
use std::path::PathBuf;

use serde_json::Value;

const PATTERNS_SHAPE: &str = "\"patterns\" must be an array of glob strings; pass an array or omit patterns for a project overview.";

pub(crate) fn tool_list(args: &Value) -> Result<String, String> {
    use globset::Glob;
    let cwd = super::require_cwd(args)?;
    let (scope, scope_warning) = super::resolve_scope(args, cwd)?;
    let budget = args.get("budget").and_then(serde_json::Value::as_u64);

    let patterns: Vec<String> = match args.get("patterns") {
        None => {
            // `scope` is an advertised parameter and is honored on the pattern
            // branch; fingerprinting `cwd` here would silently widen a scoped
            // request to the whole checkout.
            let overview = crate::overview::fingerprint(&scope);
            // fingerprint() is contractually fail-silent (it catch_unwinds to
            // an empty string) because it began life as a cosmetic initialize
            // banner. As a whole tool response that inverts tool_list's own
            // contract, so an empty result becomes an error rather than a
            // successful "this project is empty".
            if overview.trim().is_empty() {
                return Err(format!(
                    "no project overview could be generated for {}",
                    scope.display()
                ));
            }
            let mut result = scope_warning.unwrap_or_default();
            result.push_str(&overview);
            return Ok(super::apply_budget(&result, budget));
        }
        Some(value) => {
            let Some(arr) = value.as_array() else {
                return Err(PATTERNS_SHAPE.to_string());
            };
            if arr.is_empty() {
                return Err("patterns must contain at least one glob".into());
            }
            if arr.len() > 20 {
                return Err(format!(
                    "patterns limited to 20 per call (got {})",
                    arr.len()
                ));
            }
            let mut patterns = Vec::with_capacity(arr.len());
            for item in arr {
                let Some(pattern) = item.as_str() else {
                    return Err(PATTERNS_SHAPE.to_string());
                };
                patterns.push(pattern.to_string());
            }
            patterns
        }
    };

    let depth = args.get("depth").and_then(|v| {
        v.as_u64()
            .map(|d| d as usize)
            .or_else(|| v.as_f64().map(|f| f as usize))
    });

    // Walk the scope directory and collect all files matching any pattern.
    let mut matchers = Vec::with_capacity(patterns.len());
    for p in &patterns {
        let glob = Glob::new(p).map_err(|e| format!("invalid glob pattern {p:?}: {e}"))?;
        matchers.push(glob.compile_matcher());
    }

    let mut entries: Vec<(PathBuf, u64)> = Vec::new();
    let mut extensions: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut builder = ignore::WalkBuilder::new(&scope);
    builder
        .follow_links(true)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .ignore(false)
        .parents(false)
        .add_custom_ignore_filename(crate::search::TILTHIGNORE_FILE)
        .filter_entry(|entry| {
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                if let Some(name) = entry.file_name().to_str() {
                    return !crate::search::SKIP_DIRS.contains(&name);
                }
            }
            true
        });
    if let Some(d) = depth {
        builder.max_depth(Some(d));
    }
    let walker = builder.build();
    for entry in walker.filter_map(Result::ok) {
        let Some(ft) = entry.file_type() else {
            continue;
        };
        if !ft.is_file() {
            continue;
        }
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let rel = path.strip_prefix(&scope).unwrap_or(path);
        let matched = matchers.iter().any(|m| m.is_match(name) || m.is_match(rel));
        if matched {
            let bytes = entry.metadata().map_or(0, |m| m.len());
            entries.push((path.to_path_buf(), bytes));
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            extensions.insert(ext.to_string());
        }
    }

    let tree = crate::mcp::tree::render_tree(&scope, &entries);
    let mut result = scope_warning.unwrap_or_default();
    result.push_str(&tree);
    if entries.is_empty() {
        if extensions.is_empty() {
            result.push_str("\nno matches\n");
        } else {
            let exts: Vec<String> = extensions.into_iter().take(10).collect();
            let _ = write!(
                result,
                "\nno matches; found extensions: {}\n",
                exts.join(", ")
            );
        }
    }
    Ok(super::apply_budget(&result, budget))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_cwd_refused() {
        // tilth_list requires cwd — the server cannot see the caller's shell cwd,
        // so a bare list must refuse with the teaching error rather than walk the
        // server's frozen process directory (the worktree bug).
        let args = serde_json::json!({ "patterns": ["*.rs"] });
        let err = tool_list(&args).unwrap_err();
        assert!(
            err.contains("cwd") && err.contains("absolute checkout directory"),
            "bare list must refuse without cwd: {err}"
        );
    }

    #[test]
    fn relative_scope_anchors_under_cwd() {
        // A relative scope anchored to cwd must resolve under cwd (not error).
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("a.rs"), "fn a() {}\n").unwrap();
        let args = serde_json::json!({
            "patterns": ["*.rs"],
            "scope": "sub",
            "cwd": tmp.path().to_str().unwrap(),
        });
        let out = tool_list(&args).expect("relative scope + cwd resolves");
        assert!(
            out.contains("a.rs"),
            "expected listing under anchored cwd: {out}"
        );
    }

    #[test]
    fn invalid_glob_pattern_returns_error() {
        // An invalid glob must surface a specific error, not be silently
        // dropped from the matcher set.
        let tmp = tempfile::tempdir().unwrap();
        let args = serde_json::json!({
            "patterns": ["["],
            "cwd": tmp.path().to_str().unwrap(),
        });
        let err = tool_list(&args).unwrap_err();
        assert!(
            err.contains("invalid glob pattern") && err.contains('['),
            "expected invalid-glob error naming the pattern: {err}"
        );
    }
    #[test]
    fn tool_list_budget_truncates_output() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..50 {
            std::fs::write(tmp.path().join(format!("f{i}.rs")), "fn f() {}\n").unwrap();
        }
        let args = serde_json::json!({
            "patterns": ["*.rs"],
            "cwd": tmp.path().to_str().unwrap(),
            "budget": 1,
        });
        let out = tool_list(&args).expect("tool_list should succeed");
        assert!(
            out.contains("... truncated"),
            "expected truncation note: {out}"
        );
    }

    #[test]
    fn tool_list_no_match_hints_available_extensions() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "fn a() {}\n").unwrap();
        let args = serde_json::json!({
            "patterns": ["*.md"],
            "cwd": tmp.path().to_str().unwrap(),
        });
        let out = tool_list(&args).expect("tool_list should succeed");
        assert!(
            out.contains("no matches; found extensions:") && out.contains("rs"),
            "expected no-match extension hint: {out}"
        );
    }

    #[test]
    fn omitted_patterns_returns_project_overview() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join("a.rs"), "fn a() {}\n").unwrap();
        let cwd = tmp.path().to_str().unwrap();

        let omitted = tool_list(&serde_json::json!({ "cwd": cwd }))
            .expect("omitted patterns return project overview");
        let expected = crate::overview::fingerprint(tmp.path());

        assert_eq!(omitted, expected);
        assert!(omitted.starts_with("[tilth] Rust project"));
        assert!(omitted.contains("source files"));
        assert!(omitted.contains("manifest: Cargo.toml"));
        assert!(
            !omitted.contains("├──"),
            "overview must not render a tree: {omitted}"
        );
        assert!(
            !omitted.contains("a.rs"),
            "overview must not list individual files: {omitted}"
        );
    }

    /// `scope` is advertised and honored on the pattern branch; the overview
    /// branch must not silently widen a scoped request to the whole checkout.
    #[test]
    fn omitted_patterns_honors_scope() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"outer\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let inner = tmp.path().join("packages").join("api");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(
            inner.join("Cargo.toml"),
            "[package]\nname = \"scoped-inner\"\nversion = \"0.2.0\"\n",
        )
        .unwrap();
        std::fs::write(inner.join("lib.rs"), "fn inner() {}\n").unwrap();
        let cwd = tmp.path().to_str().unwrap();

        let scoped = tool_list(&serde_json::json!({ "cwd": cwd, "scope": "packages/api" }))
            .expect("scoped overview");
        let unscoped = tool_list(&serde_json::json!({ "cwd": cwd })).expect("unscoped overview");

        assert_ne!(
            scoped, unscoped,
            "a scoped overview must not equal the whole-checkout overview"
        );
        assert!(
            scoped.starts_with("[tilth] Rust project"),
            "scoped overview must render the inner package, got: {scoped}"
        );
        assert!(
            scoped.contains("manifest: Cargo.toml"),
            "scoped overview must name the inner manifest: {scoped}"
        );
        assert!(
            !scoped.contains("outer"),
            "scoped overview must not leak the outer package name: {scoped}"
        );
    }

    /// A bad scope produces a warning on the pattern branch; the overview
    /// branch must surface it too rather than dropping it.
    #[test]
    fn omitted_patterns_surfaces_scope_warning() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join("a.rs"), "fn a() {}\n").unwrap();
        let cwd = tmp.path().to_str().unwrap();

        let out = tool_list(&serde_json::json!({ "cwd": cwd, "scope": "does/not/exist" }))
            .expect("missing scope falls back with a warning");
        let bare = tool_list(&serde_json::json!({ "cwd": cwd })).expect("bare overview");
        assert_eq!(
            out,
            format!(
                "scope \"does/not/exist\" is not a valid directory, searching the cwd/checkout directory instead.\n\n{bare}"
            ),
            "warning must name the bad scope and be prepended to the overview, got: {out}"
        );
    }

    #[test]
    fn non_array_patterns_returns_teaching_error() {
        // A present-but-wrong-shape `patterns` is a caller mistake, not an
        // omission — the error must teach the expected shape.
        let tmp = tempfile::tempdir().unwrap();
        let args = serde_json::json!({
            "patterns": "not-an-array",
            "cwd": tmp.path().to_str().unwrap(),
        });
        let err = tool_list(&args).unwrap_err();
        assert!(
            err.contains("array of glob strings"),
            "expected teaching error naming the expected shape: {err}"
        );
    }

    #[test]
    fn non_string_pattern_element_returns_teaching_error() {
        // Array with non-string elements is invalid too, with the same
        // shape-teaching error.
        let tmp = tempfile::tempdir().unwrap();
        let args = serde_json::json!({
            "patterns": [42],
            "cwd": tmp.path().to_str().unwrap(),
        });
        let err = tool_list(&args).unwrap_err();
        assert!(
            err.contains("array of glob strings"),
            "expected teaching error naming the expected shape: {err}"
        );
    }

    #[test]
    fn null_patterns_returns_teaching_error() {
        // JSON null is present-but-invalid, not omission — only a truly
        // absent `patterns` key gets the ["*"] default. The error must teach
        // both the shape and the omit path.
        let tmp = tempfile::tempdir().unwrap();
        let args = serde_json::json!({
            "patterns": null,
            "cwd": tmp.path().to_str().unwrap(),
        });
        let err = tool_list(&args).unwrap_err();
        assert!(
            err.contains("array of glob strings") && err.contains("omit"),
            "null patterns must get the shape-teaching error, not the default: {err}"
        );
    }
}
