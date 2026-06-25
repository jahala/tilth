use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::cache::OutlineCache;
use crate::index::bloom::BloomFilterCache;
use crate::session::Session;

use super::{apply_budget, resolve_scope};

pub(in crate::mcp) fn tool_search(
    args: &Value,
    cache: &OutlineCache,
    session: &Session,
    bloom: &Arc<BloomFilterCache>,
) -> Result<String, String> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or("missing required parameter: query")?;
    let (scope, scope_warning) = resolve_scope(args);
    let kind = args
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("symbol");
    let expand = args
        .get("expand")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(2) as usize;
    let context_path = args
        .get("context")
        .and_then(|v| v.as_str())
        .map(PathBuf::from);
    let context = context_path.as_deref();
    let glob = args.get("glob").and_then(|v| v.as_str());
    let budget = args.get("budget").and_then(serde_json::Value::as_u64);

    // Conditional search: when `if_modified_since` is supplied, result sections
    // for files unchanged since the token collapse to a one-line stub and the
    // response carries a `{"if_modified_since":<now>}` token header. Absent the
    // param, output is byte-identical to an unconditional search (no header).
    let since = args
        .get("if_modified_since")
        .and_then(|v| v.as_str())
        .and_then(crate::mcp::iso::parse_iso_utc);
    let now = std::time::SystemTime::now();

    let output = match kind {
        "symbol" => {
            let queries: Vec<&str> = query
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            match queries.len() {
                0 => return Err("missing required parameter: query".into()),
                1 => {
                    session.record_search(queries[0]);
                    crate::search::search_symbol_expanded(
                        queries[0], &scope, cache, session, bloom, expand, context, glob, false,
                    )
                }
                2..=5 => {
                    for q in &queries {
                        session.record_search(q);
                    }
                    crate::search::search_multi_symbol_expanded(
                        &queries, &scope, cache, session, bloom, expand, context, glob, false,
                    )
                }
                _ => {
                    return Err(format!(
                        "multi-symbol search limited to 5 queries (got {})",
                        queries.len()
                    ))
                }
            }
        }
        "content" => {
            session.record_search(query);
            crate::search::search_content_expanded(
                query, &scope, cache, session, expand, context, glob, false,
            )
        }
        "regex" => {
            session.record_search(query);
            let result = crate::search::content::search(query, &scope, true, context, glob, false)
                .map_err(|e| e.to_string())?;
            crate::search::format_raw_result(&result, cache)
        }
        "callers" => {
            session.record_search(query);
            crate::search::callers::search_callers_expanded(
                query, &scope, bloom, expand, context, glob, false,
            )
        }
        _ => {
            return Err(format!(
                "unknown search kind: {kind}. Use: symbol, content, regex, callers"
            ))
        }
    }
    .map_err(|e| e.to_string())?;

    let output = match since {
        Some(s) => redact_unchanged_search_sections(&output, &scope, s),
        None => output,
    };

    let mut result = scope_warning.unwrap_or_default();
    result.push_str(&apply_budget(&output, budget));
    if since.is_some() {
        result = crate::mcp::iso::with_meta_header(Some(now), serde_json::Map::new(), &result);
    }
    Ok(result)
}

/// Replace search-result sections whose file is unchanged since `since` with a
/// one-line `# <path> (unchanged @ <ts>)` stub. A missing file is treated as
/// changed (rendered as-is, never stubbed) by `file_changed_since`.
fn redact_unchanged_search_sections(
    output: &str,
    scope: &Path,
    since: std::time::SystemTime,
) -> String {
    let mut rendered = Vec::new();
    let mut current = Vec::new();
    let mut current_path: Option<PathBuf> = None;

    for line in output.lines() {
        if is_search_section_heading(line) {
            flush_search_section(&mut rendered, &mut current, current_path.take(), since);
            current_path = search_result_path(line, scope);
        }
        current.push(line.to_string());
    }
    flush_search_section(&mut rendered, &mut current, current_path, since);
    rendered.join("\n")
}

fn flush_search_section(
    rendered: &mut Vec<String>,
    current: &mut Vec<String>,
    current_path: Option<PathBuf>,
    since: std::time::SystemTime,
) {
    if current.is_empty() {
        return;
    }
    if let Some(path) = current_path {
        if !crate::mcp::iso::file_changed_since(&path, since) {
            rendered.push(crate::mcp::iso::unchanged_stub(&path, since));
            current.clear();
            return;
        }
    }
    rendered.push(current.join("\n"));
    current.clear();
}

fn is_search_section_heading(line: &str) -> bool {
    line.starts_with("## ") || line.starts_with("### ")
}

fn search_result_path(line: &str, scope: &Path) -> Option<PathBuf> {
    let rest = line
        .strip_prefix("### ")
        .or_else(|| line.strip_prefix("## "))?;
    let loc = rest.split_whitespace().next()?;
    let (path_part, _) = loc.rsplit_once(':')?;
    if path_part.is_empty() {
        return None;
    }
    // No existence check: flush_search_section calls file_changed_since, which
    // already treats a missing file as changed (rendered as-is, never stubbed).
    Some(scope.join(path_part))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::tools::tool_definitions;

    fn search(dir: &std::path::Path, args: Value) -> String {
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        let _ = dir;
        tool_search(&args, &cache, &session, &bloom).expect("search ok")
    }

    #[test]
    fn tool_search_if_modified_since_redacts_unchanged_bodies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("target.rs");
        std::fs::write(
            &path,
            "pub fn unique_search_target() {\n    let body_marker = 7;\n}\n",
        )
        .unwrap();

        // Future token: the file's mtime is in the past relative to it, so the
        // section is unchanged and its body must be redacted to a stub.
        let out = search(
            dir.path(),
            serde_json::json!({
                "query": "unique_search_target",
                "scope": dir.path().to_str().unwrap(),
                "if_modified_since": "2099-01-01T00:00:00Z",
            }),
        );

        assert!(out.contains("unchanged"), "expected unchanged stub: {out}");
        assert!(
            !out.contains("body_marker"),
            "unchanged section body must not leak: {out}"
        );
        assert!(
            out.contains("\"if_modified_since\""),
            "cache-token header must ride along when opted in: {out}"
        );
    }

    #[test]
    fn tool_search_without_if_modified_since_emits_no_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("target.rs");
        std::fs::write(&path, "pub fn unique_search_target() {}\n").unwrap();

        let out = search(
            dir.path(),
            serde_json::json!({
                "query": "unique_search_target",
                "scope": dir.path().to_str().unwrap(),
            }),
        );

        assert!(
            !out.contains("\"if_modified_since\""),
            "no token header when the caller did not opt in: {out}"
        );
    }

    #[test]
    fn tilth_search_schema_lists_if_modified_since() {
        let tools = tool_definitions(false);
        let search = tools
            .iter()
            .find(|tool| tool.get("name").and_then(Value::as_str) == Some("tilth_search"))
            .expect("tilth_search definition");
        assert!(
            search
                .pointer("/inputSchema/properties/if_modified_since")
                .is_some(),
            "tilth_search schema must advertise if_modified_since: {search}"
        );
    }
}
