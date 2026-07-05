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

use super::resolve_scope;

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

    // Batch budget: split the total across queries so every query is
    // represented (no silent trailing drops). Per-query truncation cites the
    // total `budget` as the lever; an aggregate ceiling caps the whole batch.
    let budget = args
        .get("budget")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(crate::budget::DEFAULT_BUDGET);
    let per_query = crate::budget::item_budget(budget, queries_arr.len());

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
        for k in ["expand", "scope", "cwd", "if_modified_since", "context"] {
            if let Some(v) = args.get(k) {
                sub.insert(k.into(), v.clone());
            }
        }
        let sub_val = Value::Object(sub);
        let body = tool_search_single(&sub_val, cache, session, bloom, edit_mode, Some(per_query))?;
        let headed = format!("## query: {qstr}\n\n{body}");
        parts.push(crate::budget::apply_item(&headed, per_query, budget));
    }
    let combined = parts.join("\n\n---\n\n");
    let cwd = super::require_cwd(args)?;
    let (scope, _) = resolve_scope(args, cwd)?;
    let combined = since
        .map(|s| redact_unchanged_search_sections(&combined, &scope, s))
        .unwrap_or(combined);
    // Aggregate ceiling: the per-query split already bounds the total, but
    // header/separator overhead can nudge it over — cap once more so the
    // batch can never exceed the host response limit.
    let combined = crate::budget::apply(&combined, budget);
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
    budget: Option<u64>,
) -> Result<String, String> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or("missing required parameter: query (or queries array)")?;
    let cwd = super::require_cwd(args)?;
    let (scope, scope_warning) = resolve_scope(args, cwd)?;
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

    let output = match kind {
        None | Some("any") => {
            // Comma = multi-symbol lookup, identical to kind:symbol. Without this
            // split the merged default searches the literal "a,b" string as one
            // symbol (and as content), silently breaking the comma syntax the tool
            // schema advertises under the default mode.
            let symbols: Vec<&str> = query
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            match symbols.len() {
                0 => return Err("missing required parameter: query".into()),
                1 => {
                    session.record_search(symbols[0]);
                    search_merged_default(
                        symbols[0], &scope, cache, session, bloom, expand, context, glob,
                        edit_mode, budget,
                    )
                }
                2..=5 => {
                    for q in &symbols {
                        session.record_search(q);
                    }
                    crate::search::search_multi_symbol_expanded(
                        &symbols, &scope, cache, session, bloom, expand, context, glob, false,
                        edit_mode, budget,
                    )
                }
                _ => {
                    return Err(format!(
                        "multi-symbol search limited to 5 queries (got {})",
                        symbols.len()
                    ))
                }
            }
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
                        edit_mode, budget,
                    )
                }
                2..=5 => {
                    for q in &queries {
                        session.record_search(q);
                    }
                    crate::search::search_multi_symbol_expanded(
                        &queries, &scope, cache, session, bloom, expand, context, glob, false,
                        edit_mode, budget,
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
                query, &scope, cache, session, expand, context, glob, false, edit_mode, budget,
            )
        }
        Some("regex") => {
            session.record_search(query);
            crate::search::search_regex_expanded(
                query, &scope, cache, session, expand, context, glob, false, edit_mode, budget,
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
    result.push_str(&output);
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
    budget: Option<u64>,
) -> Result<String, crate::error::TilthError> {
    // Path-like miss suggestions: the default search returns an empty-result
    // header on a miss, so a slightly-off path (`src/serch/symbol.rs`) would
    // never reach the basic-path fuzzy fallback. For a path-like query with no
    // symbol/content match anywhere, error with the closest real file(s) as a
    // "did you mean" list — tilth never auto-opens a file the agent didn't name.
    // Gated on `is_path_like` so a normal empty search never walks the tree.
    if crate::read::fuzzy_path::is_path_like(query) {
        let sym_hits = crate::search::search_symbol_raw(query, scope, glob)?.total_found;
        let content_hits = crate::search::search_content_raw(query, scope, glob)?.total_found;
        if sym_hits == 0 && content_hits == 0 {
            if let Some(suggestions) =
                crate::read::fuzzy_path::search_miss_suggestions(scope, query)
            {
                return Err(crate::error::TilthError::NotFound {
                    path: scope.join(query),
                    suggestion: Some(suggestions.join(", ")),
                });
            }
        }
    }

    let mut sections = Vec::new();
    sections.push(format!(
        "## symbol results\n\n{}",
        crate::search::search_symbol_expanded(
            query, scope, cache, session, bloom, expand, context, glob, false, edit_mode, budget,
        )?
    ));
    sections.push(format!(
        "## content results\n\n{}",
        crate::search::search_content_expanded(
            query, scope, cache, session, expand, context, glob, false, edit_mode, budget,
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

    /// A bare `tilth_search` with no cwd must refuse: the server cannot see the
    /// caller's shell cwd, so it must be told the absolute checkout directory.
    #[test]
    fn no_cwd_refused() {
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = std::sync::Arc::new(BloomFilterCache::new());
        let args = serde_json::json!({ "queries": [{"query": "anything"}] });
        let err = tool_search(&args, &cache, &session, &bloom, false).unwrap_err();
        assert!(
            err.contains("cwd") && err.contains("absolute checkout directory"),
            "bare search must refuse without cwd: {err}"
        );
    }
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
            "cwd": tmp.path().to_str().unwrap(),
        });

        let out = tool_search(&args, &cache, &session, &bloom, false).unwrap();

        // Both targets must be reported with a real call site, not a single
        // literal "alpha,beta" lookup that finds nothing. Header uses the
        // unified single-target shape: `# Callers of "<target>" in <scope>`.
        assert!(
            out.contains("Callers of \"alpha\""),
            "missing alpha section: {out}"
        );
        assert!(
            out.contains("Callers of \"beta\""),
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

    /// The default MCP search path returns an empty-result header on a miss, so
    /// a path-like query that does not resolve to a real file would never reach
    /// the basic-path fuzzy fallback. A slightly-off path with no search matches
    /// must error with the closest real file surfaced as a "did you mean"
    /// suggestion — never auto-opening a file the agent didn't name.
    #[test]
    fn merged_default_path_like_miss_suggests_real_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src/search")).unwrap();
        std::fs::write(
            tmp.path().join("src/search/symbol.rs"),
            "pub fn find() {}\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "pub mod search;\n").unwrap();

        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = std::sync::Arc::new(BloomFilterCache::new());
        // `serch/symbol.rs` — deletion typo, path-like, single winner, no kind.
        let args = serde_json::json!({
            "queries": [{"query": "serch/symbol.rs"}],
            "scope": tmp.path().to_str().unwrap(),
            "cwd": tmp.path().to_str().unwrap(),
        });

        let err = tool_search(&args, &cache, &session, &bloom, false)
            .expect_err("path-like miss must not auto-open — suggest-only");
        assert!(
            err.contains("did you mean") && err.contains("src/search/symbol.rs"),
            "expected a did-you-mean suggestion for the real file: {err}"
        );
    }

    /// A non-path-like miss on the default MCP path must NOT walk the tree — it
    /// stays the normal empty-result response, never an error or a suggestion.
    #[test]
    fn merged_default_non_path_like_miss_stays_empty_result() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src/search")).unwrap();
        std::fs::write(
            tmp.path().join("src/search/symbol.rs"),
            "pub fn find() {}\n",
        )
        .unwrap();

        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = std::sync::Arc::new(BloomFilterCache::new());
        // `symbol` IS a subsequence of `src/search/symbol.rs` but is not
        // path-like (no separator/extension), so it isolates the `is_path_like`
        // gate — not merely the subsequence filter, which a garbage query would
        // also trip. It matches no symbol or content in the fixture either.
        let args = serde_json::json!({
            "queries": [{"query": "symbol"}],
            "scope": tmp.path().to_str().unwrap(),
            "cwd": tmp.path().to_str().unwrap(),
        });

        let out = tool_search(&args, &cache, &session, &bloom, false).unwrap();
        assert!(
            out.contains("0 matches"),
            "expected the normal empty-result response: {out}"
        );
    }

    /// Regression: a comma query under the default (merged/`any`) kind must be
    /// treated as a multi-symbol lookup, not searched as a literal "a,b" string.
    /// Before the fix `search_merged_default` passed the raw comma string to
    /// symbol + content search, so e.g. "`Planner,planning_agent`" found nothing
    /// even though `Planner` existed.
    #[test]
    fn default_comma_query_finds_both_symbols() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("lib.rs"),
            "fn alpha() {}\n\
             fn beta() {}\n",
        )
        .unwrap();

        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = std::sync::Arc::new(BloomFilterCache::new());
        // No `kind` → default merged/any path.
        let args = serde_json::json!({
            "queries": [{"query": "alpha,beta"}],
            "scope": tmp.path().to_str().unwrap(),
            "cwd": tmp.path().to_str().unwrap(),
        });

        let out = tool_search(&args, &cache, &session, &bloom, false).unwrap();

        assert!(out.contains("alpha"), "missing alpha: {out}");
        assert!(out.contains("beta"), "missing beta: {out}");
        // The literal combined string must never be searched as one symbol.
        assert!(
            !out.contains("\"alpha,beta\""),
            "comma query was treated as a literal symbol under default kind: {out}"
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
            "cwd": tmp.path().to_str().unwrap(),
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

    /// HIGH finding from PR review: the multi-target path must not silently
    /// drop the single-target path's "Adaptive 2nd-hop impact analysis".
    /// `alpha` is called by exactly `IMPACT_FANOUT_THRESHOLD`-or-fewer unique
    /// functions (one: `uses_alpha`), which are themselves called by
    /// `hop2_alpha` — so the 2-target search "alpha,beta" must show a 2nd-hop
    /// section for the alpha bucket, same as a lone `callers("alpha")` would.
    #[test]
    fn callers_multi_target_includes_second_hop_impact_per_bucket() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("lib.rs"),
            "fn alpha() {}\n\
             fn beta() {}\n\
             fn uses_alpha() { alpha(); }\n\
             fn hop2_alpha() { uses_alpha(); }\n\
             fn uses_beta() { beta(); }\n",
        )
        .unwrap();

        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = std::sync::Arc::new(BloomFilterCache::new());

        // Single-target baseline: what callers("alpha") alone produces.
        let single_args = serde_json::json!({
            "queries": [{"query": "alpha", "kind": "callers"}],
            "scope": tmp.path().to_str().unwrap(),
            "cwd": tmp.path().to_str().unwrap(),
        });
        let single_out = tool_search(&single_args, &cache, &session, &bloom, false).unwrap();
        assert!(
            single_out.contains("impact (2nd hop)"),
            "single-target baseline should show 2nd-hop impact: {single_out}"
        );
        assert!(single_out.contains("hop2_alpha"));

        // Multi-target: "alpha,beta" must not omit what a lone "alpha" search
        // would show for the alpha bucket.
        let multi_args = serde_json::json!({
            "queries": [{"query": "alpha,beta", "kind": "callers"}],
            "scope": tmp.path().to_str().unwrap(),
            "cwd": tmp.path().to_str().unwrap(),
        });
        let multi_out = tool_search(&multi_args, &cache, &session, &bloom, false).unwrap();
        assert!(
            multi_out.contains("impact (2nd hop)"),
            "multi-target alpha bucket dropped the 2nd-hop impact section: {multi_out}"
        );
        assert!(
            multi_out.contains("hop2_alpha"),
            "multi-target alpha bucket missing the hop-2 caller: {multi_out}"
        );
    }

    /// MED finding from PR review: single- and multi-target output must use
    /// the same header shape for the same target — the review found multi
    /// diverging into a `## callers of "foo"` / `### path:line` style while
    /// single used `# Callers of "foo" in <scope> — N call site(s)` /
    /// `## path:line`. A caller diffing single vs. one bucket of multi should
    /// see the identical shape (same target, same scope, same one hit).
    #[test]
    fn callers_multi_target_header_matches_single_target_shape() {
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

        let single_args = serde_json::json!({
            "queries": [{"query": "alpha", "kind": "callers"}],
            "scope": tmp.path().to_str().unwrap(),
            "cwd": tmp.path().to_str().unwrap(),
        });
        let single_out = tool_search(&single_args, &cache, &session, &bloom, false).unwrap();

        let multi_args = serde_json::json!({
            "queries": [{"query": "alpha,beta", "kind": "callers"}],
            "scope": tmp.path().to_str().unwrap(),
            "cwd": tmp.path().to_str().unwrap(),
        });
        let multi_out = tool_search(&multi_args, &cache, &session, &bloom, false).unwrap();

        // Top-level bucket header: same "# Callers of ... — N call site(s)" shape.
        assert!(
            single_out.contains("# Callers of \"alpha\""),
            "single-target header shape missing: {single_out}"
        );
        assert!(
            multi_out.contains("# Callers of \"alpha\""),
            "multi-target alpha bucket must render the single-target header shape, \
             not a divergent '## callers of' shape: {multi_out}"
        );
        assert!(
            single_out.contains("1 call site"),
            "single-target count phrase missing: {single_out}"
        );
        assert!(
            multi_out.contains("1 call site"),
            "multi-target alpha bucket must render the same count phrase: {multi_out}"
        );

        // Call-site sub-header: same "## path:line [caller: name]" shape,
        // not multi's divergent "### path:line [caller: name]".
        assert!(
            single_out.contains("[caller: uses_alpha]"),
            "single-target caller label missing: {single_out}"
        );
        assert!(
            multi_out.contains("[caller: uses_alpha]"),
            "multi-target alpha bucket must render the same caller label: {multi_out}"
        );
        assert!(
            !multi_out.contains("### lib.rs"),
            "multi-target must use single-target's '##' sub-header level, not '###': {multi_out}"
        );
    }

    /// MED finding from PR review: `BATCH_EARLY_QUIT` (50 raw matches) is a
    /// walk-wide budget shared by every target in a batch search. The walker
    /// (`find_callers_batch`) checks this budget once per **file** visited
    /// (an `AtomicUsize` compared before each file read — see
    /// `src/search/callers.rs`'s `found_count.load(..) >= early_quit_threshold`
    /// gate), so it only starves later files, not later matches within one
    /// already-open file. To reproduce real starvation this test spreads 60
    /// `alpha` call sites across 60 separate files (one call site per file:
    /// `a_00.rs`..`a_59.rs`) — comfortably above the un-scaled 50-match
    /// walk-wide budget — and puts `beta`'s lone call site in a file that
    /// sorts after all of them (`z_beta.rs`). With an unscaled budget the
    /// walk can quit after visiting ~50 of the `a_*.rs` files, before
    /// `z_beta.rs` is ever read, starving beta entirely. Scaling the budget
    /// by target count (2x for 2 targets = 100) gives the walk enough
    /// headroom to reach `z_beta.rs`.
    #[test]
    fn callers_multi_target_later_target_not_starved_by_hit_rich_earlier_target() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("defs.rs"), "fn alpha() {}\nfn beta() {}\n").unwrap();
        for i in 0..60 {
            std::fs::write(
                tmp.path().join(format!("a_{i:02}.rs")),
                format!("fn uses_alpha_{i}() {{ alpha(); }}\n"),
            )
            .unwrap();
        }
        // Sorts after every "a_*.rs" file — only reached if the walk's
        // early-quit budget has enough headroom to visit all 61 prior files.
        std::fs::write(tmp.path().join("z_beta.rs"), "fn uses_beta() { beta(); }\n").unwrap();

        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = std::sync::Arc::new(BloomFilterCache::new());
        let args = serde_json::json!({
            "queries": [{"query": "alpha,beta", "kind": "callers"}],
            "scope": tmp.path().to_str().unwrap(),
            "cwd": tmp.path().to_str().unwrap(),
        });

        let out = tool_search(&args, &cache, &session, &bloom, false).unwrap();

        assert!(
            out.contains("uses_beta"),
            "beta call site starved by alpha's hit-rich budget consumption \
             (early-quit budget was not scaled by target count): {out}"
        );
    }
}
