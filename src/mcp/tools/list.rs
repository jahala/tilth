//! `tilth_list` — tree output with token-cost rollups.
//!
//! Resolves each glob via `ignore::WalkBuilder`, collects `(path, byte_len)`
//! pairs, and renders them as a single tree rooted at scope.

use std::path::PathBuf;

use serde_json::Value;

pub(crate) fn tool_list(args: &Value) -> Result<String, String> {
    use globset::Glob;
    let (scope, scope_warning) = super::resolve_scope(args);
    let budget = args.get("budget").and_then(serde_json::Value::as_u64);

    let patterns_arr_owned: Vec<Value>;
    let patterns_arr: &Vec<Value> = match args.get("patterns") {
        Some(v) => v.as_array().ok_or("patterns must be an array of globs")?,
        None => match args.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => {
                patterns_arr_owned = vec![Value::String(p.to_string())];
                &patterns_arr_owned
            }
            None => return Err("missing required parameter: patterns".into()),
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
    let patterns: Vec<String> = patterns_arr
        .iter()
        .map(|v| {
            v.as_str()
                .ok_or("patterns must be an array of strings")
                .map(String::from)
        })
        .collect::<Result<_, _>>()?;

    let depth = args
        .get("depth")
        .and_then(serde_json::Value::as_u64)
        .map(|d| d as usize);

    // Walk the scope directory and collect all files matching any pattern.
    let matchers: Vec<_> = patterns
        .iter()
        .filter_map(|p| Glob::new(p).ok().map(|g| g.compile_matcher()))
        .collect();
    if matchers.is_empty() {
        return Err("no valid globs provided".into());
    }

    let mut entries: Vec<(PathBuf, u64)> = Vec::new();
    let walker = ignore::WalkBuilder::new(&scope)
        .follow_links(true)
        .hidden(false)
        .git_ignore(false)
        .filter_entry(|entry| {
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                if let Some(name) = entry.file_name().to_str() {
                    return !crate::search::SKIP_DIRS.contains(&name);
                }
            }
            true
        })
        .build();
    for entry in walker.filter_map(Result::ok) {
        let Some(ft) = entry.file_type() else {
            continue;
        };
        if !ft.is_file() {
            continue;
        }
        let path = entry.path();
        if let Some(d) = depth {
            let rel = path.strip_prefix(&scope).unwrap_or(path);
            let parts = rel.components().count();
            if parts > d {
                continue;
            }
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let rel = path.strip_prefix(&scope).unwrap_or(path);
        let matched = matchers.iter().any(|m| m.is_match(name) || m.is_match(rel));
        if matched {
            let bytes = entry.metadata().map_or(0, |m| m.len());
            entries.push((path.to_path_buf(), bytes));
        }
    }

    let tree = crate::mcp::tree::render_tree(&scope, &entries);
    let mut result = scope_warning.unwrap_or_default();
    result.push_str(&super::apply_budget(tree, budget));
    Ok(result)
}
