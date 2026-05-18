use std::sync::Arc;

use serde_json::Value;

use crate::index::bloom::BloomFilterCache;

use super::resolve_scope;

pub(in crate::mcp) fn tool_grok(
    args: &Value,
    bloom: &Arc<BloomFilterCache>,
) -> Result<String, String> {
    let target = args
        .get("target")
        .and_then(|v| v.as_str())
        .ok_or("missing required parameter: target")?;
    let (scope, scope_warning) = resolve_scope(args);
    let full = args.get("full").and_then(Value::as_bool).unwrap_or(false);
    let caps = if full {
        crate::search::grok::GrokCaps::full()
    } else {
        crate::search::grok::GrokCaps::default()
    };

    let result =
        crate::search::grok::grok(target, &scope, bloom, caps).map_err(|e| e.to_string())?;
    let mut output = scope_warning.unwrap_or_default();
    output.push_str(&crate::search::grok::format_grok(&result, &scope));
    Ok(output)
}
