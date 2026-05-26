use serde_json::Value;

use super::{apply_budget, resolve_scope};

pub(in crate::mcp) fn tool_files(args: &Value) -> Result<String, String> {
    let (scope, scope_warning) = resolve_scope(args);
    let budget = args.get("budget").and_then(serde_json::Value::as_u64);

    let single = args.get("pattern").and_then(|v| v.as_str());
    let patterns_arr = args.get("patterns").and_then(|v| v.as_array());

    if single.is_some() && patterns_arr.is_some() {
        return Err("provide either pattern (single) or patterns (array), not both".into());
    }

    let patterns: Vec<&str> = if let Some(arr) = patterns_arr {
        if arr.is_empty() {
            return Err("patterns must contain at least one glob".into());
        }
        if arr.len() > 20 {
            return Err(format!(
                "patterns limited to 20 per call (got {})",
                arr.len()
            ));
        }
        arr.iter()
            .map(|v| v.as_str().ok_or("patterns must be an array of strings"))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        vec![single.ok_or("missing required parameter: pattern (or use patterns for batch)")?]
    };

    let mut blocks = Vec::with_capacity(patterns.len());
    for p in &patterns {
        let block = crate::search::search_glob(p, &scope).map_err(|e| e.to_string())?;
        blocks.push(block);
    }
    let combined = blocks.join("\n\n");

    let mut result = scope_warning.unwrap_or_default();
    result.push_str(&apply_budget(&combined, budget));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a small scratch project with .rs and .toml files and return the
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
    fn tool_files_patterns_emits_one_block_per_pattern() {
        let project = scratch_project();
        let args = serde_json::json!({
            "patterns": ["*.rs", "*.toml"],
            "scope": project.path().to_str().unwrap(),
        });
        let out = tool_files(&args).expect("tool_files should succeed");
        // Two `# Glob:` headers — one per pattern.
        let header_count = out.matches("# Glob:").count();
        assert_eq!(header_count, 2, "expected 2 Glob headers, got: {out}");
        assert!(out.contains("\"*.rs\""), "missing rs header in: {out}");
        assert!(out.contains("\"*.toml\""), "missing toml header in: {out}");
        // Files from both patterns appear in the combined output.
        assert!(out.contains("main.rs"), "missing main.rs in: {out}");
        assert!(out.contains("Cargo.toml"), "missing Cargo.toml in: {out}");
    }

    #[test]
    fn tool_files_pattern_and_patterns_mutually_exclusive() {
        let args = serde_json::json!({
            "pattern": "*.rs",
            "patterns": ["*.rs"],
        });
        let err = tool_files(&args).expect_err("expected mutual-exclusion error");
        assert!(err.contains("either pattern"), "unexpected error: {err}");
    }

    #[test]
    fn tool_files_empty_patterns_errors() {
        let args = serde_json::json!({ "patterns": [] });
        let err = tool_files(&args).expect_err("expected empty-patterns error");
        assert!(err.contains("at least one"), "unexpected error: {err}");
    }

    #[test]
    fn tool_files_patterns_capped_at_20() {
        let twenty_one: Vec<&str> = vec!["*.rs"; 21];
        let args = serde_json::json!({ "patterns": twenty_one });
        let err = tool_files(&args).expect_err("expected cap error");
        assert!(err.contains("limited to 20"), "unexpected error: {err}");
    }

    #[test]
    fn tool_files_missing_pattern_and_patterns_errors() {
        let args = serde_json::json!({});
        let err = tool_files(&args).expect_err("expected missing-pattern error");
        assert!(
            err.contains("missing required parameter"),
            "unexpected error: {err}"
        );
    }
}
