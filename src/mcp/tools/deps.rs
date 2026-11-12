use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use tilth_core::index::bloom::BloomFilterCache;
use tilth_core::search::walk::WalkConfig;

use super::resolve_scope;

pub(in crate::mcp) fn tool_deps(
    args: &Value,
    bloom: &Arc<BloomFilterCache>,
    walk: WalkConfig,
    server_root: Option<&Path>,
) -> Result<String, String> {
    let path_str = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("missing required parameter: path")?;
    let root = args.get("root").and_then(|v| v.as_str()).map(Path::new);
    let path = super::resolve_read_path(&PathBuf::from(path_str), root)?;
    let (scope, scope_warning) =
        resolve_scope(args, root, server_root, crate::FileScopeHint::Directory)?;
    let budget = args
        .get("budget")
        .and_then(serde_json::Value::as_u64)
        .map(|b| b as usize);

    let deps_result =
        tilth_core::analyze_deps(&path, &scope, bloom, walk).map_err(|e| e.to_string())?;
    let mut output = scope_warning.unwrap_or_default();
    output.push_str(&crate::search::deps::format_deps(
        &deps_result,
        &scope,
        budget,
        false,
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
    fn relative_path_no_root_errors() {
        // WHY: tilth_deps resolves its `path` arg through resolve_read_path. A
        // relative path with no absolute root silently resolved against the
        // frozen server cwd before this spec. The `?` on the path resolution must
        // propagate the refusal, naming the path and the root escape hatch.
        let args = serde_json::json!({ "path": "src/foo.rs" });
        let err = tool_deps(&args, &bloom(), WalkConfig::default(), None).unwrap_err();
        assert!(
            err.contains("src/foo.rs") && err.contains("root"),
            "relative deps path without root must refuse: {err}"
        );
    }

    #[test]
    fn absolute_path_omitted_scope_no_root_defaults_to_the_server_root() {
        // WHY: the require-root discipline fires ONLY when a caller EXPLICITLY
        // passes a relative path/scope without an absolute root. `scope` is
        // never required by tilth_deps — an absolute `path` with an omitted
        // `scope` must resolve scope to the server's root, not refuse. That is
        // the default flow of path-only tilth_deps calls.
        let tmp = tempfile::tempdir().unwrap();
        let abs = tmp.path().join("foo.rs");
        std::fs::write(&abs, "fn foo() {}\n").unwrap();
        let args = serde_json::json!({ "path": abs.to_str().unwrap() });
        let out = tool_deps(&args, &bloom(), WalkConfig::default(), Some(tmp.path()))
            .expect("absolute path + omitted scope must default to the server root, not refuse");
        assert!(
            !out.contains("cannot be resolved"),
            "unexpected refusal: {out}"
        );
    }

    #[test]
    fn absolute_path_explicit_relative_scope_no_root_errors() {
        // An EXPLICITLY passed relative `scope` with no absolute root is
        // unresolvable (the server cannot see the caller's shell cwd) — this
        // must still refuse, even though `path` is absolute.
        let tmp = tempfile::tempdir().unwrap();
        let abs = tmp.path().join("foo.rs");
        std::fs::write(&abs, "fn foo() {}\n").unwrap();
        let args = serde_json::json!({
            "path": abs.to_str().unwrap(),
            "scope": "some/relative/dir",
        });
        let err = tool_deps(&args, &bloom(), WalkConfig::default(), None).unwrap_err();
        assert!(
            err.contains("relative scope") && err.contains("root"),
            "explicit relative scope without root must refuse: {err}"
        );
    }

    /// #195: a file passed as `scope` used to fall back to the whole root.
    #[test]
    fn file_scope_with_root_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("src")).unwrap();
        let abs = tmp.path().join("src/foo.rs");
        std::fs::write(&abs, "pub fn foo() {}\n").unwrap();
        let args = serde_json::json!({
            "path": abs.to_str().unwrap(),
            "scope": "src/foo.rs",
            "root": tmp.path().to_str().unwrap(),
        });
        let err = tool_deps(&args, &bloom(), WalkConfig::default(), None)
            .expect_err("a file scope must be refused, not widened to root");
        assert!(
            err.contains("scope \"src/foo.rs\" is a file")
                && err.contains("scope to its directory (\"src\")"),
            "refusal must name the file and its directory: {err}"
        );
        assert!(
            !err.contains("glob"),
            "tilth_deps has no glob parameter to suggest: {err}"
        );
    }
}
