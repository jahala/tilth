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
    let root = args
        .get("root")
        .and_then(|v| v.as_str())
        .map(std::path::Path::new);
    let (scope, scope_warning) = resolve_scope(args, root)?;
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
                        budget,
                    )
                }
                2..=5 => {
                    for q in &queries {
                        session.record_search(q);
                    }
                    crate::search::search_multi_symbol_expanded(
                        &queries, &scope, cache, session, bloom, expand, context, glob, false,
                        budget,
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
                query, &scope, cache, session, expand, context, glob, false, budget,
            )
        }
        "regex" => {
            session.record_search(query);
            let result = crate::search::content::search(query, &scope, true, context, glob, false)
                .map_err(|e| e.to_string())?;
            crate::search::format_raw_result(&result, cache)
        }
        "callers" => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::OutlineCache;
    use crate::index::bloom::BloomFilterCache;
    use crate::session::Session;

    /// Regression: `kind=callers` with a comma query must search each target
    /// separately, not for a literal symbol named "alpha,beta". Before the
    /// comma-split arm this returned an empty no-callers message.
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
            "query": "alpha,beta",
            "kind": "callers",
            "scope": tmp.path().to_str().unwrap(),
        });

        let out = tool_search(&args, &cache, &session, &bloom).unwrap();

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
            "query": "alpha,alpha",
            "kind": "callers",
            "scope": tmp.path().to_str().unwrap(),
        });

        let out = tool_search(&args, &cache, &session, &bloom).unwrap();

        assert!(
            out.contains("uses_alpha"),
            "alpha call site not found: {out}"
        );
        // The duplicate must collapse to a single section: no no-callers
        // message should appear — that is what the second occurrence rendered
        // before the dedupe consumed the bucket on the first pass.
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
            "query": "alpha",
            "kind": "callers",
            "scope": tmp.path().to_str().unwrap(),
        });
        let single_out = tool_search(&single_args, &cache, &session, &bloom).unwrap();
        assert!(
            single_out.contains("impact (2nd hop)"),
            "single-target baseline should show 2nd-hop impact: {single_out}"
        );
        assert!(single_out.contains("hop2_alpha"));

        // Multi-target: "alpha,beta" must not omit what a lone "alpha" search
        // would show for the alpha bucket.
        let multi_args = serde_json::json!({
            "query": "alpha,beta",
            "kind": "callers",
            "scope": tmp.path().to_str().unwrap(),
        });
        let multi_out = tool_search(&multi_args, &cache, &session, &bloom).unwrap();
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
            "query": "alpha",
            "kind": "callers",
            "scope": tmp.path().to_str().unwrap(),
        });
        let single_out = tool_search(&single_args, &cache, &session, &bloom).unwrap();

        let multi_args = serde_json::json!({
            "query": "alpha,beta",
            "kind": "callers",
            "scope": tmp.path().to_str().unwrap(),
        });
        let multi_out = tool_search(&multi_args, &cache, &session, &bloom).unwrap();

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
            "query": "alpha,beta",
            "kind": "callers",
            "scope": tmp.path().to_str().unwrap(),
        });

        let out = tool_search(&args, &cache, &session, &bloom).unwrap();

        assert!(
            out.contains("uses_beta"),
            "beta call site starved by alpha's hit-rich budget consumption \
             (early-quit budget was not scaled by target count): {out}"
        );
    }

    /// WHY: the require-root discipline fires ONLY when a caller EXPLICITLY
    /// passes a relative scope/path without an absolute root. A bare
    /// `tilth_search(query)` call with no scope is the default flow of every
    /// session and must keep working exactly as it does on main — refusing
    /// here would break every session's default search. This inverts the PR's
    /// original (too strict) assertion.
    ///
    /// Asserts only `is_ok()`, not the response body: the body is real search
    /// output over whatever tree the test runs in (including this very source
    /// file), so substring-matching it is not a reliable way to detect a
    /// require-root refusal. `resolve_scope`'s own unit tests in
    /// `mcp::tools::tests` already pin the exact refusal-vs-default-cwd
    /// behavior directly; this test only pins that `tool_search` propagates
    /// success through to its caller instead of swallowing it into an error.
    #[test]
    fn no_scope_no_root_defaults_to_cwd() {
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        let args = serde_json::json!({ "query": "anything_unlikely_to_match_zzz" });
        let result = tool_search(&args, &cache, &session, &bloom);
        assert!(
            result.is_ok(),
            "bare search must default to cwd, not refuse: {result:?}"
        );
    }

    /// An EXPLICITLY passed relative scope with no absolute root to anchor it
    /// is unresolvable (the server cannot see the caller's shell cwd) — this
    /// must still refuse.
    #[test]
    fn explicit_relative_scope_no_root_errors() {
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        let args = serde_json::json!({ "query": "anything", "scope": "some/relative/dir" });
        let err = tool_search(&args, &cache, &session, &bloom).unwrap_err();
        assert!(
            err.contains("relative scope") && err.contains("root"),
            "explicit relative scope without root must refuse: {err}"
        );
    }
}
