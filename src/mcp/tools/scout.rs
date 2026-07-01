use std::sync::Arc;

use serde_json::Value;

use crate::index::bloom::BloomFilterCache;
use crate::session::Session;

use super::resolve_scope;

pub(in crate::mcp) fn tool_scout(
    args: &Value,
    _bloom: &Arc<BloomFilterCache>,
    _session: &Session,
) -> Result<String, String> {
    let prompt = args
        .get("prompt")
        .and_then(|v| v.as_str())
        .ok_or("missing required parameter: prompt")?;
    let (scope, scope_warning) = resolve_scope(args);
    // Default to "rerank" so the full validated pipeline fires when models are
    // present; degrades automatically to deterministic context when absent.
    let job = args.get("job").and_then(|v| v.as_str()).unwrap_or("rerank");

    // Return JSON so the caller can inspect gate_fired, skeleton, n_pool.
    let result = crate::run_scout(prompt, &scope, job, true).map_err(|e| e.to_string())?;
    // Volatile bytes poison prompt-prefix caching — identical calls must return
    // identical results, so timing stays off the MCP surface (the CLI keeps it).
    let result = match serde_json::from_str::<Value>(&result) {
        Ok(mut v) => {
            if let Some(o) = v.as_object_mut() {
                o.remove("elapsed_ms");
            }
            serde_json::to_string_pretty(&v).unwrap_or(result)
        }
        Err(_) => result,
    };
    let mut output = scope_warning.unwrap_or_default();
    output.push_str(&result);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &std::path::Path, rel: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn tool_scout_missing_prompt_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let bloom = Arc::new(BloomFilterCache::new());
        let session = crate::session::Session::new();
        let args = serde_json::json!({
            "scope": tmp.path().to_str().unwrap(),
        });
        let err = tool_scout(&args, &bloom, &session).expect_err("missing prompt must error");
        assert!(
            err.contains("missing required parameter: prompt"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn tool_scout_returns_json_for_valid_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "src/parse.rs",
            "pub fn parse_unified_diff(input: &str) -> Vec<u8> { vec![] }\n",
        );
        let bloom = Arc::new(BloomFilterCache::new());
        let session = crate::session::Session::new();
        let args = serde_json::json!({
            "prompt": "parse a unified diff",
            "scope": tmp.path().to_str().unwrap(),
            "job": "context",
        });
        // Should succeed — output is JSON.
        let out = tool_scout(&args, &bloom, &session).expect("scout should succeed");
        // Verify output is parseable JSON with expected fields.
        let v: serde_json::Value =
            serde_json::from_str(&out).expect("tool_scout must return valid JSON");
        assert!(
            v.get("candidates").is_some(),
            "JSON must have candidates field: {out}"
        );
        assert!(
            v.get("gate_fired").is_some(),
            "JSON must have gate_fired field: {out}"
        );
    }

    #[test]
    fn tool_scout_defaults_job_to_rerank() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "a.rs", "fn foo() {}\n");
        let bloom = Arc::new(BloomFilterCache::new());
        let session = crate::session::Session::new();
        // No "job" arg — should default to "rerank" (degrades gracefully without models).
        let args = serde_json::json!({
            "prompt": "foo function",
            "scope": tmp.path().to_str().unwrap(),
        });
        let out = tool_scout(&args, &bloom, &session).expect("default job=rerank should succeed");
        let v: serde_json::Value = serde_json::from_str(&out).expect("output must be JSON");
        assert!(v.get("candidates").is_some(), "must have candidates: {out}");
    }
}
