use std::sync::Arc;

use serde_json::Value;

use crate::session::Session;
use tilth_core::index::bloom::BloomFilterCache;

use super::resolve_scope;

pub(in crate::mcp) fn tool_grok(
    args: &Value,
    bloom: &Arc<BloomFilterCache>,
    session: &Session,
) -> Result<String, String> {
    let target = args
        .get("target")
        .and_then(|v| v.as_str())
        .ok_or("missing required parameter: target")?;
    let root = args
        .get("root")
        .and_then(|v| v.as_str())
        .map(std::path::Path::new);
    let (scope, scope_warning) = resolve_scope(args, root, crate::FileScopeHint::Directory)?;
    let full = args.get("full").and_then(Value::as_bool).unwrap_or(false);
    let caps = if full {
        crate::search::grok::GrokCaps::full()
    } else {
        crate::search::grok::GrokCaps::default()
    };

    let result = crate::search::grok::grok(target, &scope, bloom, session, caps)
        .map_err(|e| e.to_string())?;
    let mut output = scope_warning.unwrap_or_default();
    output.push_str(&crate::search::grok::format_grok(&result, &scope));
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #195: a file passed as `scope` used to fall back to the whole root.
    /// `tilth_grok` has no glob parameter, so the hint names only the directory.
    #[test]
    fn file_scope_with_root_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/x.py"), "def needle():\n    pass\n").unwrap();
        let args = serde_json::json!({
            "target": "needle",
            "scope": "src/x.py",
            "root": tmp.path().to_str().unwrap(),
        });
        let bloom = Arc::new(BloomFilterCache::new());
        let err = tool_grok(&args, &bloom, &Session::new())
            .expect_err("a file scope must be refused, not widened to root");
        assert!(
            err.contains("scope \"src/x.py\" is a file")
                && err.contains("scope to its directory (\"src\")"),
            "refusal must name the file and its directory: {err}"
        );
        assert!(
            !err.contains("glob"),
            "tilth_grok has no glob parameter to suggest: {err}"
        );
    }
}
