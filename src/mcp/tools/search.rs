use std::path::PathBuf;
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

    let mut result = scope_warning.unwrap_or_default();
    result.push_str(&apply_budget(&output, budget));
    Ok(result)
}
