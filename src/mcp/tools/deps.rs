//! `tilth_deps` — file-level dependency analysis (imports + dependents).

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::index::bloom::BloomFilterCache;

pub(crate) fn tool_deps(args: &Value, bloom: &Arc<BloomFilterCache>) -> Result<String, String> {
    let path_str = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("missing required parameter: path")?;
    let path = PathBuf::from(path_str);
    let (scope, scope_warning) = super::resolve_scope(args);
    let budget = args
        .get("budget")
        .and_then(serde_json::Value::as_u64)
        .map(|b| b as usize);

    let deps_result =
        crate::search::deps::analyze_deps(&path, &scope, bloom).map_err(|e| e.to_string())?;
    let mut output = scope_warning.unwrap_or_default();
    output.push_str(&crate::search::deps::format_deps(
        &deps_result,
        &scope,
        budget,
    ));
    Ok(output)
}
