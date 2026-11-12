use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::session::Session;
use tilth_core::cache::OutlineCache;
use tilth_core::index::bloom::BloomFilterCache;
use tilth_core::search::walk::{Walk, WalkConfig};

use super::{apply_budget, resolve_scope};

#[expect(clippy::too_many_lines, reason = "split by responsibility in #315")]
pub(in crate::mcp) fn tool_search(
    args: &Value,
    cache: &OutlineCache,
    session: &Session,
    bloom: &Arc<BloomFilterCache>,
    config: WalkConfig,
) -> Result<String, String> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or("missing required parameter: query")?;
    let root = args
        .get("root")
        .and_then(|v| v.as_str())
        .map(std::path::Path::new);
    let (scope, scope_warning) = resolve_scope(args, root, crate::FileScopeHint::Glob)?;
    let kind = args
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("symbol");
    let expand = args
        .get("expand")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(2) as usize;
    let full = args.get("full").and_then(Value::as_bool).unwrap_or(false);
    let context_path = args
        .get("context")
        .and_then(|v| v.as_str())
        .map(PathBuf::from);
    let context = context_path.as_deref();
    let walk = Walk {
        scope: &scope,
        glob: args.get("glob").and_then(|v| v.as_str()),
        config,
    };
    // Expanded search and the response cap must use the same budget.
    let budget = Some(
        args.get("budget")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(crate::budget::DEFAULT_BUDGET),
    );

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
                        queries[0], walk, cache, session, bloom, expand, context, full, budget,
                    )
                }
                2..=5 => {
                    for q in &queries {
                        session.record_search(q);
                    }
                    crate::search::search_multi_symbol_expanded(
                        &queries, walk, cache, session, bloom, expand, context, full, budget,
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
                query, walk, cache, session, expand, context, full, budget,
            )
        }
        "regex" => {
            session.record_search(query);
            let result = crate::search::content::search(query, walk, true, context, full)
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
                        targets[0], walk, bloom, expand, context, full,
                    )
                }
                2..=5 => {
                    for t in &targets {
                        session.record_search(t);
                    }
                    crate::search::callers::search_callers_multi_expanded(
                        &targets, walk, bloom, expand, context, full,
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
    use crate::session::Session;
    use tilth_core::cache::OutlineCache;
    use tilth_core::index::bloom::BloomFilterCache;

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

        let out = tool_search(&args, &cache, &session, &bloom, WalkConfig::default()).unwrap();

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

        let out = tool_search(&args, &cache, &session, &bloom, WalkConfig::default()).unwrap();

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
        let single_out = tool_search(
            &single_args,
            &cache,
            &session,
            &bloom,
            WalkConfig::default(),
        )
        .unwrap();
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
        let multi_out =
            tool_search(&multi_args, &cache, &session, &bloom, WalkConfig::default()).unwrap();
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
        let single_out = tool_search(
            &single_args,
            &cache,
            &session,
            &bloom,
            WalkConfig::default(),
        )
        .unwrap();

        let multi_args = serde_json::json!({
            "query": "alpha,beta",
            "kind": "callers",
            "scope": tmp.path().to_str().unwrap(),
        });
        let multi_out =
            tool_search(&multi_args, &cache, &session, &bloom, WalkConfig::default()).unwrap();

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

    /// A multi-target callers search walks once for all targets. When that
    /// walk quit on a shared count of 50 sites, 60 `alpha` call sites in 60
    /// files could use it up before the walk reached `z_beta.rs`, and `beta`
    /// came back with no callers. The walk now visits every file and counts
    /// each target on its own.
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
        // Sorts after every "a_*.rs" file.
        std::fs::write(tmp.path().join("z_beta.rs"), "fn uses_beta() { beta(); }\n").unwrap();

        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = std::sync::Arc::new(BloomFilterCache::new());
        let args = serde_json::json!({
            "query": "alpha,beta",
            "kind": "callers",
            "scope": tmp.path().to_str().unwrap(),
        });

        let out = tool_search(&args, &cache, &session, &bloom, WalkConfig::default()).unwrap();

        assert!(
            out.contains("uses_beta"),
            "beta call site starved by alpha's hit-rich budget consumption: {out}"
        );
        assert!(
            out.contains("— 10 of 60 call sites"),
            "alpha's total counts every file: {out}"
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
        let result = tool_search(&args, &cache, &session, &bloom, WalkConfig::default());
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
        let err = tool_search(&args, &cache, &session, &bloom, WalkConfig::default()).unwrap_err();
        assert!(
            err.contains("relative scope") && err.contains("root"),
            "explicit relative scope without root must refuse: {err}"
        );
    }

    /// A checkout with the same symbol in two sibling files, so a search that
    /// widens past the file an agent named is visible in the output.
    fn two_file_checkout() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/x.py"), "def needle():\n    pass\n").unwrap();
        std::fs::write(tmp.path().join("src/y.py"), "def needle():\n    pass\n").unwrap();
        tmp
    }

    /// #195: a file passed as `scope` together with an absolute `root` used
    /// to succeed with a warning and search the whole root. Agents treat a
    /// successful response as authoritative, so a narrowing argument must
    /// never widen. The refusal names the file and the call that does work.
    #[test]
    fn file_scope_with_root_is_refused_with_glob_hint() {
        let tmp = two_file_checkout();
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        let args = serde_json::json!({
            "query": "needle",
            "scope": "src/x.py",
            "root": tmp.path().to_str().unwrap(),
        });
        let err = tool_search(&args, &cache, &session, &bloom, WalkConfig::default())
            .expect_err("a file scope must be refused, not widened to root");
        assert!(
            err.contains("scope \"src/x.py\" is a file"),
            "refusal must name the file: {err}"
        );
        assert!(
            err.contains("scope to its directory (\"src\")")
                && err.contains("glob to its name (\"x.py\")"),
            "refusal must name the working call: {err}"
        );
    }

    /// The absolute-file case without `root` already refused, but its message
    /// blamed the missing root. It must name the real problem and the fix.
    #[test]
    fn absolute_file_scope_without_root_names_the_fix() {
        let tmp = two_file_checkout();
        let file = tmp.path().join("src/x.py");
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        let args = serde_json::json!({ "query": "needle", "scope": file.to_str().unwrap() });
        let err = tool_search(&args, &cache, &session, &bloom, WalkConfig::default()).unwrap_err();
        let dir = tmp.path().join("src");
        assert!(
            err.contains("is a file")
                && err.contains(&format!("(\"{}\")", dir.display()))
                && err.contains("glob to its name (\"x.py\")"),
            "refusal must name the directory and glob fix: {err}"
        );
    }

    /// The refusal's advice must actually work: scope at the directory plus
    /// glob at the name searches that one file and not its sibling.
    #[test]
    fn file_scope_hint_narrows_to_the_one_file() {
        let tmp = two_file_checkout();
        let out = run_search(&serde_json::json!({
            "query": "needle",
            "scope": "src",
            "glob": "x.py",
            "root": tmp.path().to_str().unwrap(),
        }));
        assert!(
            out.contains("x.py"),
            "the named file must be searched: {out}"
        );
        assert!(
            !out.contains("y.py"),
            "the sibling must stay out of a one-file search: {out}"
        );
    }

    /// A scope that does not exist at all (usually a typo) keeps the soft
    /// fallback: search root and say so. This is deliberate, so it is pinned.
    #[test]
    fn missing_dir_scope_with_root_still_falls_back_with_warning() {
        let tmp = two_file_checkout();
        let out = run_search(&serde_json::json!({
            "query": "needle",
            "scope": "no/such/dir",
            "root": tmp.path().to_str().unwrap(),
        }));
        assert!(
            out.starts_with(
                "scope \"no/such/dir\" is not a valid directory, searching the root/checkout directory instead."
            ),
            "a missing directory must warn and fall back to root: {out}"
        );
        assert!(
            out.contains("x.py") && out.contains("y.py"),
            "the fallback must search the whole root: {out}"
        );
    }

    /// How many of the `n` numbered `<prefix>_NN.rs` fixture files the output
    /// names. Counting file names instead of a header phrase keeps these
    /// assertions independent of the header and facet wording.
    fn files_shown(out: &str, prefix: &str, n: usize) -> usize {
        (0..n)
            .filter(|i| out.contains(&format!("{prefix}_{i:02}.rs")))
            .count()
    }

    /// Write `n` files named `<prefix>_NN.rs`, one match per file, so a cap
    /// can be counted by how many of them the output names.
    fn spread(dir: &std::path::Path, prefix: &str, n: usize, body: impl Fn(usize) -> String) {
        for i in 0..n {
            std::fs::write(dir.join(format!("{prefix}_{i:02}.rs")), body(i)).unwrap();
        }
    }

    #[test]
    fn later_search_expands_a_definition_omitted_by_the_budget() {
        use std::fmt::Write as _;

        let tmp = tempfile::tempdir().unwrap();
        let mut source = String::from("pub fn big_target() {\n");
        for i in 0..400 {
            let _ = writeln!(source, "    let v{i} = {i} + 1;");
        }
        source.push_str("}\n\nfn user() {\n    big_target();\n}\n");
        std::fs::write(tmp.path().join("big.rs"), source).unwrap();

        check_budgeted_expansion(
            tmp.path(),
            "big_target",
            300,
            "big_target",
            "let v0 =",
            "expansion omitted to fit budget",
        );
    }

    #[test]
    fn later_search_expands_a_definition_cut_from_a_multi_search() {
        let tmp = tempfile::tempdir().unwrap();
        for (file, symbol, marker) in [
            ("first.rs", "first_target", "marker_one"),
            ("second.rs", "second_target", "marker_two"),
        ] {
            let source = format!("pub fn {symbol}() {{\n    let {marker} = 1;\n}}\n");
            std::fs::write(tmp.path().join(file), source).unwrap();
        }

        check_budgeted_expansion(
            tmp.path(),
            "first_target,second_target",
            100,
            "second_target",
            "let marker_two =",
            "... truncated (",
        );
    }

    fn check_budgeted_expansion(
        scope: &std::path::Path,
        tight_query: &str,
        tight_budget: u64,
        next_query: &str,
        source: &str,
        omission: &str,
    ) {
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        let search = |query: &str, budget: u64| {
            let args = serde_json::json!({ "query": query, "scope": scope, "budget": budget });
            tool_search(&args, &cache, &session, &bloom, WalkConfig::default()).unwrap()
        };

        let tight = search(tight_query, tight_budget);
        assert!(
            tight.contains(omission) && !tight.contains(source),
            "{tight}"
        );
        let roomy = search(next_query, 10_000);
        assert!(
            roomy.contains(source) && !roomy.contains("[shown earlier]"),
            "{roomy}"
        );
        let repeated = search(next_query, 10_000);
        assert!(
            repeated.contains("[shown earlier]") && !repeated.contains(source),
            "{repeated}"
        );
    }

    #[test]
    fn repeated_symbol_is_deduped_only_if_the_earlier_section_kept_its_body() {
        use std::fmt::Write as _;

        let tmp = tempfile::tempdir().unwrap();
        let mut source = String::from("pub fn big_target() {\n");
        for i in 0..400 {
            let _ = writeln!(source, "    let marker_{i} = {i} + 1;");
        }
        source.push_str("}\n");
        std::fs::write(tmp.path().join("big.rs"), source).unwrap();

        let search = |budget: u64| {
            run_search(&serde_json::json!({
                "query": "big_target,big_target",
                "scope": tmp.path(),
                "expand": 2,
                "budget": budget,
            }))
        };

        let tight = search(240);
        assert_eq!(tight.matches("# Search:").count(), 2, "{tight}");
        assert!(tight.contains("expansion omitted to fit budget"), "{tight}");
        assert!(!tight.contains("let marker_0 ="), "{tight}");
        assert!(!tight.contains("[shown earlier]"), "{tight}");

        let roomy = search(3000);
        assert_eq!(roomy.matches("let marker_0 =").count(), 1, "{roomy}");
        assert_eq!(roomy.matches("[shown earlier]").count(), 1, "{roomy}");
    }

    fn run_search(args: &Value) -> String {
        let cache = OutlineCache::new();
        let session = Session::new();
        let bloom = Arc::new(BloomFilterCache::new());
        tool_search(args, &cache, &session, &bloom, WalkConfig::default()).unwrap()
    }

    /// `full` must reach both symbol dispatches. The handler hard-coded
    /// `false` for every kind, so an agent could never see past the 10-match
    /// cap that `--full` already lifts on the CLI. The cap is per query, so a
    /// comma search must widen every section, not share one budget.
    #[test]
    fn full_raises_symbol_match_cap_per_query() {
        let tmp = tempfile::tempdir().unwrap();
        spread(tmp.path(), "syma", 15, |_| {
            "pub fn AlphaThing() {}\n".to_string()
        });
        spread(tmp.path(), "symb", 15, |_| {
            "pub fn BetaThing() {}\n".to_string()
        });

        // Single query.
        let mut args = serde_json::json!({
            "query": "AlphaThing",
            "scope": tmp.path().to_str().unwrap(),
        });
        let capped = run_search(&args);
        assert_eq!(
            files_shown(&capped, "syma", 15),
            10,
            "default must render exactly the 10-match cap: {capped}"
        );
        args["full"] = Value::Bool(true);
        let widened = run_search(&args);
        assert!(
            files_shown(&widened, "syma", 15) > 10,
            "full must raise the symbol cap past 10: {widened}"
        );

        // Comma query — a separate dispatch with its own `full` argument.
        let mut multi = serde_json::json!({
            "query": "AlphaThing,BetaThing",
            "scope": tmp.path().to_str().unwrap(),
        });
        let capped_multi = run_search(&multi);
        assert_eq!(
            (
                files_shown(&capped_multi, "syma", 15),
                files_shown(&capped_multi, "symb", 15)
            ),
            (10, 10),
            "default must cap each comma section at 10: {capped_multi}"
        );
        multi["full"] = Value::Bool(true);
        let widened_multi = run_search(&multi);
        assert!(
            files_shown(&widened_multi, "syma", 15) > 10
                && files_shown(&widened_multi, "symb", 15) > 10,
            "full must raise the cap in every comma section: {widened_multi}"
        );
    }

    /// Same for `kind=content`, which dispatches through
    /// `search_content_expanded`'s own `full` parameter.
    #[test]
    fn full_raises_content_match_cap() {
        let tmp = tempfile::tempdir().unwrap();
        spread(tmp.path(), "cnt", 15, |_| {
            "fn f() { let _ = ScatteredNeedle; }\n".to_string()
        });

        let mut args = serde_json::json!({
            "query": "ScatteredNeedle",
            "kind": "content",
            "scope": tmp.path().to_str().unwrap(),
        });

        let capped = run_search(&args);
        assert_eq!(
            files_shown(&capped, "cnt", 15),
            10,
            "default must render exactly the 10-match cap: {capped}"
        );

        args["full"] = Value::Bool(true);
        let widened = run_search(&args);
        assert!(
            files_shown(&widened, "cnt", 15) > 10,
            "full must raise the content cap past 10: {widened}"
        );
    }

    /// `kind=regex` shares `content::search` with `kind=content` but pins the
    /// pattern as a regex. The query below matches only under regex
    /// semantics, so this pins the `is_regex` argument as well as the `full`
    /// one — the two are adjacent bools at the same call.
    #[test]
    fn full_raises_regex_match_cap() {
        let tmp = tempfile::tempdir().unwrap();
        spread(tmp.path(), "rgx", 15, |_| {
            "fn f() { let _ = NeedleQ7; }\n".to_string()
        });

        let mut args = serde_json::json!({
            "query": "Needle[A-Z][0-9]",
            "kind": "regex",
            "scope": tmp.path().to_str().unwrap(),
        });

        let capped = run_search(&args);
        assert_eq!(
            files_shown(&capped, "rgx", 15),
            10,
            "default must render exactly the 10-match cap, and only a regex \
             match reaches 10 at all: {capped}"
        );

        args["full"] = Value::Bool(true);
        let widened = run_search(&args);
        assert!(
            files_shown(&widened, "rgx", 15) > 10,
            "full must raise the regex cap past 10: {widened}"
        );
    }

    /// Same for both `kind=callers` dispatches, whose cap and walk budget
    /// both key off `full`. As with symbol search the cap is per target.
    #[test]
    fn full_raises_callers_match_cap_per_target() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("defs.rs"), "fn alpha() {}\nfn beta() {}\n").unwrap();
        spread(tmp.path(), "cala", 15, |i| {
            format!("fn a_uses_{i:02}() {{ alpha(); }}\n")
        });
        spread(tmp.path(), "calb", 15, |i| {
            format!("fn b_uses_{i:02}() {{ beta(); }}\n")
        });

        // Single target.
        let mut args = serde_json::json!({
            "query": "alpha",
            "kind": "callers",
            "scope": tmp.path().to_str().unwrap(),
        });
        let capped = run_search(&args);
        assert_eq!(
            files_shown(&capped, "cala", 15),
            10,
            "default must render exactly the 10-call-site cap: {capped}"
        );
        args["full"] = Value::Bool(true);
        let widened = run_search(&args);
        assert!(
            files_shown(&widened, "cala", 15) > 10,
            "full must raise the callers cap past 10: {widened}"
        );

        // Comma query — a separate dispatch with its own `full` argument.
        let mut multi = serde_json::json!({
            "query": "alpha,beta",
            "kind": "callers",
            "scope": tmp.path().to_str().unwrap(),
        });
        let capped_multi = run_search(&multi);
        assert_eq!(
            (
                files_shown(&capped_multi, "cala", 15),
                files_shown(&capped_multi, "calb", 15)
            ),
            (10, 10),
            "default must cap each target bucket at 10: {capped_multi}"
        );
        multi["full"] = Value::Bool(true);
        let widened_multi = run_search(&multi);
        assert!(
            files_shown(&widened_multi, "cala", 15) > 10
                && files_shown(&widened_multi, "calb", 15) > 10,
            "full must raise the cap in every target bucket: {widened_multi}"
        );
    }

    /// The schema must advertise `full`, or no agent can reach the widened cap.
    #[test]
    fn tilth_search_schema_advertises_full() {
        let search = crate::mcp::tools::tool_definitions(false)
            .into_iter()
            .find(|t| t.get("name").and_then(Value::as_str) == Some("tilth_search"))
            .expect("tilth_search definition");
        assert_eq!(
            search.pointer("/inputSchema/properties/full/type"),
            Some(&Value::from("boolean")),
            "tilth_search must advertise a boolean `full`: {search}"
        );
    }
}
