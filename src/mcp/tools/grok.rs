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
        .ok_or(
            "missing required parameter: target (symbol name or \"path:line\", e.g. \"Type::method\" or \"src/file.rs:7\")",
        )?;
    let cwd = super::require_cwd(args)?;
    let (scope, scope_warning) = resolve_scope(args, cwd)?;
    let budget = args
        .get("budget")
        .and_then(Value::as_u64)
        .unwrap_or(crate::budget::DEFAULT_BUDGET);
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
    Ok(crate::budget::apply(&output, budget))
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
    fn missing_target_teaches_grok_target_grammar() {
        let err = tool_grok(&serde_json::json!({}), &bloom(), &Session::new()).unwrap_err();
        assert!(
            err.contains("missing required parameter: target")
                && err.contains("\"Type::method\"")
                && err.contains("\"src/file.rs:7\""),
            "missing target should teach the target grammar: {err}"
        );
    }

    #[test]
    fn non_string_target_uses_same_teaching_error() {
        let expected = tool_grok(&serde_json::json!({}), &bloom(), &Session::new()).unwrap_err();
        for target in [
            serde_json::Value::Null,
            serde_json::json!(42),
            serde_json::json!(true),
        ] {
            let err = tool_grok(
                &serde_json::json!({ "target": target }),
                &bloom(),
                &Session::new(),
            )
            .unwrap_err();
            assert_eq!(
                err, expected,
                "invalid target {target} should teach the same call"
            );
        }
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

    #[test]
    fn budget_truncates_output() {
        // A tiny budget must shrink the response relative to the default;
        // grok previously ignored `budget` entirely and always returned the
        // full result.
        let args_full = serde_json::json!({ "target": "grok", "cwd": env!("CARGO_MANIFEST_DIR") });
        let args_small = serde_json::json!({
            "target": "grok",
            "cwd": env!("CARGO_MANIFEST_DIR"),
            "budget": 50
        });
        let full = tool_grok(&args_full, &bloom(), &Session::new()).expect("full grok succeeds");
        let small =
            tool_grok(&args_small, &bloom(), &Session::new()).expect("budgeted grok succeeds");
        assert!(
            small.contains("... truncated — raise `budget`"),
            "budget=50 output should hit the truncation marker: {small}"
        );
    }
}
