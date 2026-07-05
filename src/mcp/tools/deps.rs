use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::index::bloom::BloomFilterCache;

use super::resolve_scope;

pub(in crate::mcp) fn tool_deps(
    args: &Value,
    bloom: &Arc<BloomFilterCache>,
) -> Result<String, String> {
    let path_str = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("missing required parameter: path")?;
    let cwd = super::require_cwd(args)?;
    let path = super::resolve_anchored(&PathBuf::from(path_str), cwd)?;
    let (scope, scope_warning) = resolve_scope(args, cwd)?;
    let budget = usize::try_from(
        args.get("budget")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(crate::budget::DEFAULT_BUDGET),
    )
    .unwrap_or(usize::MAX);

    let deps_result =
        crate::search::deps::analyze_deps(&path, &scope, bloom).map_err(|e| e.to_string())?;
    let mut output = scope_warning.unwrap_or_default();
    output.push_str(&crate::search::deps::format_deps(
        &deps_result,
        &scope,
        Some(budget),
    ));
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bloom() -> Arc<BloomFilterCache> {
        Arc::new(BloomFilterCache::new())
    }

    #[test]
    fn no_cwd_refused() {
        // tilth_deps requires cwd. A relative `path` with no cwd must refuse
        // with the teaching error rather than resolve against the server cwd.
        let args = serde_json::json!({ "path": "src/foo.rs" });
        let err = tool_deps(&args, &bloom()).unwrap_err();
        assert!(
            err.contains("cwd") && err.contains("absolute checkout directory"),
            "relative deps path without cwd must refuse: {err}"
        );
    }

    #[test]
    fn relative_path_anchors_under_cwd() {
        // A relative `path` anchors under cwd; an omitted scope defaults to cwd.
        // The dependents search runs from cwd, not the server's frozen cwd.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("foo.rs"), "fn foo() {}\n").unwrap();
        let args = serde_json::json!({
            "path": "foo.rs",
            "cwd": tmp.path().to_str().unwrap()
        });
        // Resolves and runs without error (the file exists under cwd).
        tool_deps(&args, &bloom()).expect("relative path anchors under cwd");
    }
}
