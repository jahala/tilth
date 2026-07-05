use std::sync::Arc;

use serde_json::Value;

use crate::index::bloom::BloomFilterCache;
use crate::session::Session;

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
    let cwd = super::require_cwd(args)?;
    let (scope, scope_warning) = resolve_scope(args, cwd)?;
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

    fn bloom() -> Arc<BloomFilterCache> {
        Arc::new(BloomFilterCache::new())
    }

    #[test]
    fn no_cwd_refused() {
        // tilth_grok requires cwd. A target with no cwd must refuse with the
        // teaching error rather than resolve scope against the server's cwd.
        let args = serde_json::json!({ "target": "Foo" });
        let err = tool_grok(&args, &bloom(), &Session::new()).unwrap_err();
        assert!(
            err.contains("cwd") && err.contains("absolute checkout directory"),
            "grok without cwd must refuse with the teaching error: {err}"
        );
    }

    #[test]
    fn relative_cwd_refused() {
        // A relative cwd reintroduces the frozen-server-cwd hazard and must be
        // refused even when a target is present.
        let args = serde_json::json!({ "target": "Foo", "cwd": "relative/dir" });
        let err = tool_grok(&args, &bloom(), &Session::new()).unwrap_err();
        assert!(
            err.contains("relative") && err.contains("absolute checkout directory"),
            "grok with a relative cwd must refuse: {err}"
        );
    }
}
