//! `tilth_search` — code search dispatcher. Supports `queries: [...]` batch
//! form plus the legacy single-query form, with per-kind routing
//! (symbol / any / content / regex / callers) and `if_modified_since`
//! redaction of unchanged result sections.

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
    edit_mode: bool,
) -> Result<String, String> {
    let now = std::time::SystemTime::now();
    let since = args
        .get("if_modified_since")
        .and_then(|v| v.as_str())
        .and_then(crate::mcp::iso::parse_iso_utc);

    // Single surface: `queries: [{query, glob?, kind?}]`. Each entry runs
    // independently; per-entry glob/kind override the top-level values.
    // Results concatenate under `## query: <q>` headers.
    let queries_arr = args
        .get("queries")
        .and_then(|v| v.as_array())
        .ok_or("missing required parameter: queries (array of {query, glob?, kind?} objects)")?;
    if queries_arr.is_empty() {
        return Err("queries array is empty".into());
    }
    if queries_arr.len() > 10 {
        return Err(format!(
            "queries array limited to 10 entries (got {})",
            queries_arr.len()
        ));
    }
    let mut parts: Vec<String> = Vec::with_capacity(queries_arr.len());
    for (i, q) in queries_arr.iter().enumerate() {
        let qstr = q
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("queries[{i}]: missing 'query' string"))?;
        let mut sub = serde_json::Map::new();
        sub.insert("query".into(), Value::String(qstr.to_string()));
        if let Some(g) = q.get("glob").and_then(|v| v.as_str()) {
            sub.insert("glob".into(), Value::String(g.to_string()));
        } else if let Some(g) = args.get("glob").and_then(|v| v.as_str()) {
            sub.insert("glob".into(), Value::String(g.to_string()));
        }
        let kind = q
            .get("kind")
            .and_then(|v| v.as_str())
            .or_else(|| args.get("kind").and_then(|v| v.as_str()));
        if let Some(kind) = kind {
            sub.insert("kind".into(), Value::String(kind.to_string()));
        }
        for k in ["expand", "scope", "budget", "if_modified_since"] {
            if let Some(v) = args.get(k) {
                sub.insert(k.into(), v.clone());
            }
        }
        let sub_val = Value::Object(sub);
        let body = tool_search_single(&sub_val, cache, session, bloom, edit_mode)?;
        parts.push(format!("## query: {qstr}\n\n{body}"));
    }
    let combined = parts.join("\n\n---\n\n");
    let (scope, _) = resolve_scope(args);
    let combined = since
        .map(|s| redact_unchanged_search_sections(&combined, &scope, s))
        .unwrap_or(combined);
    // Per-entry budget caps each query in isolation; cap the concatenated
    // batch once more so an N-entry batch can't return ~N× the budget.
    let combined = apply_budget(
        &combined,
        args.get("budget").and_then(serde_json::Value::as_u64),
    );
    Ok(crate::mcp::iso::with_meta_header(
        Some(now),
        serde_json::Map::new(),
        &combined,
    ))
}

fn tool_search_single(
    args: &Value,
    cache: &OutlineCache,
    session: &Session,
    bloom: &Arc<BloomFilterCache>,
    edit_mode: bool,
) -> Result<String, String> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or("missing required parameter: query (or queries array)")?;
    let (scope, scope_warning) = resolve_scope(args);
    let kind = args.get("kind").and_then(|v| v.as_str());
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
        None | Some("any") => {
            session.record_search(query);
            search_merged_default(
                query, &scope, cache, session, bloom, expand, context, glob, edit_mode,
            )
        }
        Some("symbol") => {
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
                        edit_mode,
                    )
                }
                2..=5 => {
                    for q in &queries {
                        session.record_search(q);
                    }
                    crate::search::search_multi_symbol_expanded(
                        &queries, &scope, cache, session, bloom, expand, context, glob, false,
                        edit_mode,
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
        Some("content") => {
            session.record_search(query);
            crate::search::search_content_expanded(
                query, &scope, cache, session, expand, context, glob, false, edit_mode,
            )
        }
        Some("regex") => {
            session.record_search(query);
            crate::search::search_regex_expanded(
                query, &scope, cache, session, expand, context, glob, false, edit_mode,
            )
        }
        Some("callers") => {
            let targets: Vec<&str> = query
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            match targets.len() {
                0 => return Err("missing required parameter: query".into()),
                1 => {
                    session.record_search(targets[0]);
                    crate::search::callers::search_callers_expanded(
                        targets[0], &scope, bloom, expand, context, glob, false,
                    )
                }
                2..=5 => {
                    for t in &targets {
                        session.record_search(t);
                    }
                    crate::search::callers::search_callers_multi_expanded(
                        &targets, &scope, bloom, expand, context, glob, false,
                    )
                }
                _ => {
                    return Err(format!(
                        "multi-target callers search limited to 5 queries (got {})",
                        targets.len()
                    ))
                }
            }
        }
        Some(kind) => {
            return Err(format!(
                "unknown search kind: {kind}. Use: symbol, any, content, regex, callers"
            ))
        }
    }
    .map_err(|e| e.to_string())?;

    let mut result = scope_warning.unwrap_or_default();
    result.push_str(&apply_budget(&output, budget));
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn search_merged_default(
    query: &str,
    scope: &Path,
    cache: &OutlineCache,
    session: &Session,
    bloom: &Arc<BloomFilterCache>,
    expand: usize,
    context: Option<&Path>,
    glob: Option<&str>,
    edit_mode: bool,
) -> Result<String, crate::error::TilthError> {
    let mut sections = Vec::new();
    sections.push(format!(
        "## symbol results\n\n{}",
        crate::search::search_symbol_expanded(
            query, scope, cache, session, bloom, expand, context, glob, false, edit_mode,
        )?
    ));
    sections.push(format!(
        "## content results\n\n{}",
        crate::search::search_content_expanded(
            query, scope, cache, session, expand, context, glob, false, edit_mode,
        )?
    ));
    if crate::classify::is_identifier(query) {
        sections.push(format!(
            "## caller results\n\n{}",
            crate::search::callers::search_callers_expanded(
                query, scope, bloom, expand, context, glob, false,
            )?
        ));
    }
    Ok(sections.join("\n\n---\n\n"))
}

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
    use crate::cache::OutlineCache;
    use crate::index::bloom::BloomFilterCache;
    use crate::session::Session;

    /// Regression for P0-3: `kind=callers` with a comma query must search each
    /// target separately, not for a literal symbol named "alpha,beta". Before
    /// the fix this returned an empty no-callers message ~70% of real sessions.
    #[test]
    fn callers_comma_query_finds_both_targets() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("lib.rs"),
            "fn alpha() {}\n\
             fn beta() {}\n\
             fn uses_alpha() { alpha(); }\n\
             fn uses_beta() { beta(); }\n",
        )
        .unwrap();

        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = std::sync::Arc::new(BloomFilterCache::new());
        let args = serde_json::json!({
            "queries": [{"query": "alpha,beta", "kind": "callers"}],
            "scope": tmp.path().to_str().unwrap(),
        });

        let out = tool_search(&args, &cache, &session, &bloom, false).unwrap();

        // Both targets must be reported with a real call site, not a single
        // literal "alpha,beta" lookup that finds nothing.
        assert!(
            out.contains("callers of \"alpha\""),
            "missing alpha section: {out}"
        );
        assert!(
            out.contains("callers of \"beta\""),
            "missing beta section: {out}"
        );
        assert!(
            out.contains("uses_alpha"),
            "alpha call site not found: {out}"
        );
        assert!(out.contains("uses_beta"), "beta call site not found: {out}");
        // The literal combined string must never be searched as one symbol.
        assert!(
            !out.contains("\"alpha,beta\""),
            "comma query was treated as a literal symbol: {out}"
        );
    }

    /// Regression for the duplicate-target render bug: query "alpha,alpha"
    /// must still report alpha's call site once, not render an empty
    /// no-callers section on the second occurrence after the first consumed
    /// the matched bucket.
    #[test]
    fn callers_duplicate_target_does_not_render_empty_section() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("lib.rs"),
            "fn alpha() {}\n\
             fn uses_alpha() { alpha(); }\n",
        )
        .unwrap();

        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = std::sync::Arc::new(BloomFilterCache::new());
        let args = serde_json::json!({
            "queries": [{"query": "alpha,alpha", "kind": "callers"}],
            "scope": tmp.path().to_str().unwrap(),
        });

        let out = tool_search(&args, &cache, &session, &bloom, false).unwrap();

        assert!(
            out.contains("uses_alpha"),
            "alpha call site not found: {out}"
        );
        // The duplicate must collapse to a single section: no no-callers
        // message should appear — that is what the second occurrence rendered
        // before the fix consumed the bucket on the first pass.
        assert!(
            !out.contains("no call sites") && !out.contains("no direct call sites"),
            "duplicate target rendered a false no-callers section: {out}"
        );
    }
}
