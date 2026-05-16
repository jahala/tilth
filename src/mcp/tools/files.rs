//! `tilth_files` — legacy glob-based file finder. Folded into `tilth_search`
//! at the MCP surface; the function body is kept under `#[cfg(test)]` so the
//! removal guarantees (tool not advertised, transitional aliases still parse)
//! remain regression-tested.

#![cfg(test)]

use serde_json::Value;

pub(crate) fn tool_files(args: &Value) -> Result<String, String> {
    let (scope, scope_warning) = super::resolve_scope(args);
    let budget = args.get("budget").and_then(serde_json::Value::as_u64);

    // Accept singular `pattern:` as a transitional alias (97% of agents use it).
    let patterns_arr_owned: Vec<Value>;
    let patterns_arr: &Vec<Value> = match args.get("patterns") {
        Some(v) => v.as_array().ok_or(
            "patterns must be an array of globs (use single-element array for one pattern)",
        )?,
        None => match args.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => {
                patterns_arr_owned = vec![Value::String(p.to_string())];
                &patterns_arr_owned
            }
            None => {
                return Err("missing required parameter: patterns (array of globs)".into());
            }
        },
    };

    if patterns_arr.is_empty() {
        return Err("patterns must contain at least one glob".into());
    }
    if patterns_arr.len() > 20 {
        return Err(format!(
            "patterns limited to 20 per call (got {})",
            patterns_arr.len()
        ));
    }
    let patterns: Vec<&str> = patterns_arr
        .iter()
        .map(|v| v.as_str().ok_or("patterns must be an array of strings"))
        .collect::<Result<Vec<_>, _>>()?;

    let mut blocks = Vec::with_capacity(patterns.len());
    for p in &patterns {
        let block = crate::search::search_glob(p, &scope).map_err(|e| e.to_string())?;
        blocks.push(block);
    }
    let combined = blocks.join("\n\n");

    let mut result = scope_warning.unwrap_or_default();
    result.push_str(&super::apply_budget(combined, budget));
    Ok(result)
}
