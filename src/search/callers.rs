use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use streaming_iterator::StreamingIterator;

use crate::error::TilthError;
use crate::lang::detect_file_type;
use crate::lang::outline::outline_language;
use crate::types::FileType;

const MAX_MATCHES: usize = 10;
/// Max unique caller functions to trace for 2nd hop. Above this = wide fan-out, skip.
const IMPACT_FANOUT_THRESHOLD: usize = 10;
/// Max 2nd-hop results to display.
const IMPACT_MAX_RESULTS: usize = 15;
/// Stop the batch caller walk once we have this many raw matches. Generous headroom for dedup + ranking.
pub(crate) const BATCH_EARLY_QUIT: usize = 50;

/// Match-count cap when `--full` is set. Mirrors the symbol/content search caps.
const FULL_MAX_MATCHES: usize = 100;
/// Walker early-quit threshold when `--full` is set.
const FULL_BATCH_EARLY_QUIT: usize = FULL_MAX_MATCHES * 3;

/// Scale a single-target batch-walk budget for a multi-target search.
///
/// `find_callers_batch`'s `early_quit_threshold` is a walk-wide raw-match
/// count shared by every target in the `HashSet` passed to it — the walker
/// has no concept of "budget per target," it just stops once the total
/// match count crosses the threshold (see `found_count` in
/// `find_callers_batch`). A single target's budget (`BATCH_EARLY_QUIT` /
/// `FULL_BATCH_EARLY_QUIT`) sized for one symbol therefore starves later
/// targets in a multi-target search once an earlier, hit-rich target
/// consumes it. Scaling linearly by target count gives each target
/// approximately its own full budget's worth of headroom; `n_targets` is
/// already bounded to 5 by the dispatch layer (`tool_search`'s
/// `2..=5 => ...` arm), so the scaled result stays bounded too.
///
/// Note: the early-quit mechanism itself is a coarse walk-wide heuristic
/// that is a candidate for removal/replacement in a future change — this
/// scaling is a minimal parity fix so multi-target does not regress vs. N
/// separate single-target calls, not a long-term investment in the
/// mechanism's design.
fn scaled_batch_quit(base_quit: usize, n_targets: usize) -> usize {
    base_quit.saturating_mul(n_targets.max(1))
}

/// A single caller match — a call site of a target symbol.
#[derive(Debug)]
pub struct CallerMatch {
    pub path: PathBuf,
    pub line: u32,
    pub calling_function: String,
    pub call_text: String,
    /// Line range of the calling function (for expand).
    pub caller_range: Option<(u32, u32)>,
    /// File content, already read during `find_callers_batch` — avoids re-reading during expand.
    /// Shared across all call sites in the same file via reference counting.
    pub content: Arc<String>,
}

/// Scan `scope` for the literal `target` byte sequence. Used by the
/// single-symbol `search_callers_expanded` path to distinguish "typo,
/// doesn't exist" from "real symbol with no direct callers" (indirect
/// dispatch, dead code, framework registration, …) when the caller walk
/// returned zero matches. mmap is lazy, so the scan only pages in regions
/// that contain the needle prefix.
fn target_seen_in_scope(target: &str, scope: &Path, glob: Option<&str>) -> bool {
    let Ok(walker) = super::walker(scope, glob) else {
        return false;
    };
    let needle = target.as_bytes();
    let seen = AtomicBool::new(false);

    walker.run(|| {
        let seen = &seen;
        Box::new(move |entry| {
            if seen.load(Ordering::Relaxed) {
                return ignore::WalkState::Quit;
            }
            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }
            let path = entry.path();
            let Ok(file) = std::fs::File::open(path) else {
                return ignore::WalkState::Continue;
            };
            let Ok(mmap) = (unsafe { memmap2::Mmap::map(&file) }) else {
                return ignore::WalkState::Continue;
            };
            if memchr::memmem::find(&mmap, needle).is_some() {
                seen.store(true, Ordering::Relaxed);
                return ignore::WalkState::Quit;
            }
            ignore::WalkState::Continue
        })
    });

    seen.load(Ordering::Relaxed)
}

/// Find all call sites of any symbol in `targets` across the codebase using a single walk.
/// Returns tuples of (`target_name`, match) so callers know which symbol was matched.
pub(crate) fn find_callers_batch(
    targets: &HashSet<String>,
    scope: &Path,
    bloom: &crate::index::bloom::BloomFilterCache,
    glob: Option<&str>,
    early_quit_threshold: usize,
) -> Result<Vec<(String, CallerMatch)>, TilthError> {
    let matches: Mutex<Vec<(String, CallerMatch)>> = Mutex::new(Vec::new());
    let found_count = AtomicUsize::new(0);

    let walker = super::walker(scope, glob)?;

    walker.run(|| {
        let matches = &matches;
        let found_count = &found_count;

        Box::new(move |entry| {
            // Early termination: enough callers found
            if found_count.load(Ordering::Relaxed) >= early_quit_threshold {
                return ignore::WalkState::Quit;
            }

            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };

            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return ignore::WalkState::Continue;
            }

            let path = entry.path();

            // Read + size-gate + bloom prefilter in one shared step.
            let Some((content, _mtime)) = super::bloom_walk::read_with_bloom_check(
                path,
                targets,
                bloom,
                super::bloom_walk::MAX_FILE_SIZE,
            ) else {
                return ignore::WalkState::Continue;
            };

            // Fast byte check via memchr::memmem (SIMD) — cheap second pass that
            // eliminates bloom false positives before tree-sitter parses.
            if !targets
                .iter()
                .any(|t| memchr::memmem::find(content.as_bytes(), t.as_bytes()).is_some())
            {
                return ignore::WalkState::Continue;
            }

            // Only process files with tree-sitter grammars
            let file_type = detect_file_type(path);
            let FileType::Code(lang) = file_type else {
                return ignore::WalkState::Continue;
            };

            let Some(ts_lang) = outline_language(lang) else {
                return ignore::WalkState::Continue;
            };

            let content = Arc::new(content);
            let file_callers =
                find_callers_treesitter_batch(path, targets, &ts_lang, &content, lang);

            if !file_callers.is_empty() {
                found_count.fetch_add(file_callers.len(), Ordering::Relaxed);
                let mut all = matches
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                all.extend(file_callers);
            }

            ignore::WalkState::Continue
        })
    });

    Ok(matches
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
}

/// Tree-sitter call site detection for a set of target symbols.
/// Returns tuples of (`matched_target_name`, `CallerMatch`).
fn find_callers_treesitter_batch(
    path: &Path,
    targets: &HashSet<String>,
    ts_lang: &tree_sitter::Language,
    content: &Arc<String>,
    lang: crate::types::Lang,
) -> Vec<(String, CallerMatch)> {
    // Get the query string for this language
    let Some(query_str) = super::callee_query::callee_query_str(lang) else {
        return Vec::new();
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(ts_lang).is_err() {
        return Vec::new();
    }

    let content_str = content.as_str();
    let Some(tree) = parser.parse(content_str, None) else {
        return Vec::new();
    };

    let content_bytes = content_str.as_bytes();
    let lines: Vec<&str> = content_str.lines().collect();

    let Some(callers) = super::callee_query::with_callee_query(ts_lang, query_str, |query| {
        let Some(callee_idx) = query.capture_index_for_name("callee") else {
            return Vec::new();
        };

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), content_bytes);
        let mut callers = Vec::new();

        while let Some(m) = matches.next() {
            for cap in m.captures {
                if cap.index != callee_idx {
                    continue;
                }

                // Check if the captured text matches any of our target symbols
                let Ok(text) = cap.node.utf8_text(content_bytes) else {
                    continue;
                };

                if !targets.contains(text) {
                    continue;
                }

                let matched_target = text.to_string();

                // Found a call site! Now walk up to find the calling function
                let line = cap.node.start_position().row as u32 + 1;

                // Get the call text (the whole call expression, not just the callee)
                let call_node = cap.node.parent().unwrap_or(cap.node);
                let same_line = call_node.start_position().row == call_node.end_position().row;
                let call_text: String = if same_line {
                    let row = call_node.start_position().row;
                    if row < lines.len() {
                        lines[row].trim().to_string()
                    } else {
                        matched_target.clone()
                    }
                } else {
                    matched_target.clone()
                };

                // Walk up the tree to find the enclosing function
                let (calling_function, caller_range) =
                    find_enclosing_function(cap.node, &lines, lang);

                callers.push((
                    matched_target,
                    CallerMatch {
                        path: path.to_path_buf(),
                        line,
                        calling_function,
                        call_text,
                        caller_range,
                        content: Arc::clone(content),
                    },
                ));
            }
        }

        callers
    }) else {
        return Vec::new();
    };

    callers
}

/// Walk up the AST from a node to find the enclosing function definition.
/// Returns (`function_name`, `line_range`). Top-level renders as `"<top-level>"`.
fn find_enclosing_function(
    node: tree_sitter::Node,
    lines: &[&str],
    lang: crate::types::Lang,
) -> (String, Option<(u32, u32)>) {
    match super::scope::walk_to_enclosing_definition(node, lines, lang) {
        Some((_, name, range)) => (name, Some(range)),
        None => ("<top-level>".to_string(), None),
    }
}

/// Format and rank caller search results with optional expand.
pub fn search_callers_expanded(
    target: &str,
    scope: &Path,
    bloom: &crate::index::bloom::BloomFilterCache,
    expand: usize,
    context: Option<&Path>,
    glob: Option<&str>,
    full: bool,
) -> Result<String, TilthError> {
    let (max_matches, batch_quit) = if full {
        (FULL_MAX_MATCHES, FULL_BATCH_EARLY_QUIT)
    } else {
        (MAX_MATCHES, BATCH_EARLY_QUIT)
    };
    let single: HashSet<String> = std::iter::once(target.to_string()).collect();
    let raw = find_callers_batch(&single, scope, bloom, glob, batch_quit)?;
    let callers: Vec<CallerMatch> = raw.into_iter().map(|(_, m)| m).collect();

    if callers.is_empty() {
        let target_seen = target_seen_in_scope(target, scope, glob);
        return Ok(no_callers_message(target, scope, target_seen, glob));
    }

    // Sort by relevance (context file first, then by proximity)
    let mut sorted_callers = callers;
    rank_callers(&mut sorted_callers, scope, context);

    let total = sorted_callers.len();

    // Collect unique caller names BEFORE truncation for accurate fan-out threshold
    let all_caller_names: HashSet<String> = sorted_callers
        .iter()
        .filter(|c| c.calling_function != "<top-level>")
        .map(|c| c.calling_function.clone())
        .collect();

    sorted_callers.truncate(max_matches);

    let mut output = String::new();
    write_caller_bucket(&mut output, target, scope, total, &sorted_callers, expand);
    write_second_hop_impact(
        &mut output,
        &all_caller_names,
        &sorted_callers,
        scope,
        bloom,
        glob,
        batch_quit,
    );

    let tokens = crate::types::estimate_tokens(output.len() as u64);
    let token_str = if tokens >= 1000 {
        format!("~{}.{}k", tokens / 1000, (tokens % 1000) / 100)
    } else {
        format!("~{tokens}")
    };
    let _ = write!(output, "\n\n({token_str} tokens)");
    Ok(output)
}

/// Render one target's caller bucket in the canonical shape shared by both
/// the single-target and multi-target callers search: a
/// `# Callers of "<target>" in <scope> — N call site(s)` header, then one
/// `## <path>:<line> [caller: <fn>]` block per call site (with an optional
/// expanded source excerpt). Multi-target search calls this once per target
/// so a bucket inside a comma query renders byte-identically to what a lone
/// single-target search of the same symbol, scope, and hits would produce.
fn write_caller_bucket(
    output: &mut String,
    target: &str,
    scope: &Path,
    total: usize,
    sorted_callers: &[CallerMatch],
    expand: usize,
) {
    let _ = writeln!(
        output,
        "# Callers of \"{}\" in {} — {} call site{}",
        target,
        scope.display(),
        total,
        if total == 1 { "" } else { "s" }
    );

    for (i, caller) in sorted_callers.iter().enumerate() {
        // Header: file:line [caller: calling_function]
        let _ = write!(
            output,
            "\n## {}:{} [caller: {}]\n",
            caller
                .path
                .strip_prefix(scope)
                .unwrap_or(&caller.path)
                .display(),
            caller.line,
            caller.calling_function
        );

        // Show the call text
        let _ = writeln!(output, "-> {}", caller.call_text);

        // Expand if requested and we have the range
        if i < expand {
            if let Some((start, end)) = caller.caller_range {
                // Use cached content — no re-read needed
                let lines: Vec<&str> = caller.content.lines().collect();
                let start_idx = (start as usize).saturating_sub(1);
                let end_idx = (end as usize).min(lines.len());

                output.push('\n');
                output.push_str("```\n");

                for (idx, line) in lines[start_idx..end_idx].iter().enumerate() {
                    let line_num = start_idx + idx + 1;
                    let prefix = if line_num == caller.line as usize {
                        "> "
                    } else {
                        "  "
                    };
                    let _ = writeln!(output, "{prefix}{line_num:4} | {line}");
                }

                output.push_str("```\n");
            }
        }
    }
}

/// Adaptive 2nd-hop impact analysis, shared by single- and multi-target
/// callers search (extracted so multi-target reuses this exact block per
/// target bucket instead of re-implementing it — PR #138 review HIGH
/// finding: the multi-target path originally omitted this entirely).
///
/// `all_caller_names` must be the target's unique direct-caller names
/// collected BEFORE `sorted_callers` truncation, so the fan-out threshold
/// check reflects the true hop-1 breadth rather than the display-capped one.
fn write_second_hop_impact(
    output: &mut String,
    all_caller_names: &HashSet<String>,
    sorted_callers: &[CallerMatch],
    scope: &Path,
    bloom: &crate::index::bloom::BloomFilterCache,
    glob: Option<&str>,
    batch_quit: usize,
) {
    if all_caller_names.is_empty() || all_caller_names.len() > IMPACT_FANOUT_THRESHOLD {
        return;
    }
    let Ok(hop2) = find_callers_batch(all_caller_names, scope, bloom, glob, batch_quit) else {
        return;
    };

    // Filter out hop-1 matches (same file+line = same call site)
    let hop1_locations: HashSet<(PathBuf, u32)> = sorted_callers
        .iter()
        .map(|c| (c.path.clone(), c.line))
        .collect();

    let hop2_filtered: Vec<_> = hop2
        .into_iter()
        .filter(|(_, m)| !hop1_locations.contains(&(m.path.clone(), m.line)))
        .collect();

    if hop2_filtered.is_empty() {
        return;
    }

    output.push_str("\n-- impact (2nd hop) --\n");

    let mut seen: HashSet<(String, PathBuf)> = HashSet::new();
    let mut count = 0;
    for (via, m) in &hop2_filtered {
        let key = (m.calling_function.clone(), m.path.clone());
        if !seen.insert(key) {
            continue;
        }
        if count >= IMPACT_MAX_RESULTS {
            break;
        }

        let rel_path = m.path.strip_prefix(scope).unwrap_or(&m.path).display();
        let _ = writeln!(
            output,
            "  {:<20} {}:{}  -> {}",
            m.calling_function, rel_path, m.line, via
        );
        count += 1;
    }

    let unique_total = hop2_filtered
        .iter()
        .map(|(_, m)| (&m.calling_function, &m.path))
        .collect::<HashSet<_>>()
        .len();
    if unique_total > IMPACT_MAX_RESULTS {
        let _ = writeln!(
            output,
            "  ... and {} more",
            unique_total - IMPACT_MAX_RESULTS
        );
    }

    let _ = writeln!(output, "\n{} functions affected across 2 hops.", {
        // Use pre-truncation distinct-caller count so footer is accurate even
        // when >max_matches hop-1 callers exist.
        all_caller_names.len() + unique_total
    });
}

/// Multi-target caller search: find call sites of 2..=5 symbols in a single
/// walk via `find_callers_batch`, then render one labeled section per target.
/// Mirrors `search_multi_symbol_expanded` for the `kind=callers` comma path.
///
/// Each target's bucket renders via the same `write_caller_bucket` +
/// `write_second_hop_impact` helpers the single-target path uses, so a
/// bucket here is byte-identical to what a lone `search_callers_expanded`
/// call for that target would produce (PR #138 review: HIGH — 2nd-hop parity;
/// MED — header shape parity). The batch walk's early-quit budget is scaled
/// by target count so a hit-rich earlier target cannot starve a later,
/// rarer one (PR #138 review: MED — budget scaling).
pub fn search_callers_multi_expanded(
    targets: &[&str],
    scope: &Path,
    bloom: &crate::index::bloom::BloomFilterCache,
    expand: usize,
    context: Option<&Path>,
    glob: Option<&str>,
    full: bool,
) -> Result<String, TilthError> {
    let (max_matches, base_batch_quit) = if full {
        (FULL_MAX_MATCHES, FULL_BATCH_EARLY_QUIT)
    } else {
        (MAX_MATCHES, BATCH_EARLY_QUIT)
    };

    // Dedupe targets, preserving first-seen order: a repeated target (e.g.
    // query "foo,foo") must not render an empty no-callers section on its
    // second occurrence after the first consumed the matched bucket. The
    // deduped list also feeds the batch search, so the input is deduped once.
    let mut seen: HashSet<&str> = HashSet::new();
    let ordered: Vec<&str> = targets
        .iter()
        .copied()
        .filter(|t| seen.insert(*t))
        .collect();

    // Scale the walk-wide early-quit budget by (deduped) target count so
    // each target gets roughly its own single-target budget's headroom —
    // see `scaled_batch_quit` for why an unscaled shared budget starves
    // later targets.
    let batch_quit = scaled_batch_quit(base_batch_quit, ordered.len());

    let target_set: HashSet<String> = ordered.iter().map(ToString::to_string).collect();
    let raw = find_callers_batch(&target_set, scope, bloom, glob, batch_quit)?;

    // Bucket matches by which target they call. Preserve the caller-supplied
    // target order so output is deterministic.
    let mut by_target: std::collections::HashMap<String, Vec<CallerMatch>> =
        std::collections::HashMap::new();
    for (name, m) in raw {
        by_target.entry(name).or_default().push(m);
    }

    let mut output = String::new();
    for target in &ordered {
        let mut callers = by_target.remove(*target).unwrap_or_default();

        if callers.is_empty() {
            let target_seen = target_seen_in_scope(target, scope, glob);
            output.push_str(&no_callers_message(target, scope, target_seen, glob));
            output.push_str("\n\n");
            continue;
        }

        rank_callers(&mut callers, scope, context);
        let total = callers.len();

        // Unique direct-caller names BEFORE truncation, same as the
        // single-target path — feeds the 2nd-hop fan-out threshold check
        // with the true hop-1 breadth rather than the display-capped one.
        let all_caller_names: HashSet<String> = callers
            .iter()
            .filter(|c| c.calling_function != "<top-level>")
            .map(|c| c.calling_function.clone())
            .collect();

        callers.truncate(max_matches);

        write_caller_bucket(&mut output, target, scope, total, &callers, expand);
        write_second_hop_impact(
            &mut output,
            &all_caller_names,
            &callers,
            scope,
            bloom,
            glob,
            batch_quit,
        );
        output.push('\n');
    }

    let tokens = crate::types::estimate_tokens(output.len() as u64);
    let token_str = if tokens >= 1000 {
        format!("~{}.{}k", tokens / 1000, (tokens % 1000) / 100)
    } else {
        format!("~{tokens}")
    };
    let _ = write!(output, "\n({token_str} tokens)");
    Ok(output)
}

/// Build the user-facing message when callers search returns no hits.
/// Splits two cases that mean very different things to an agent:
/// `target_seen = true` means the symbol exists somewhere but has no direct
/// call sites — probable indirect dispatch, so we show a richer hint
/// listing the common indirection mechanisms. `target_seen = false` means
/// the literal name never appears in scope — most often a typo or wrong
/// scope, so we suppress the indirect-dispatch hint to avoid misleading
/// the agent.
fn no_callers_message(target: &str, scope: &Path, target_seen: bool, glob: Option<&str>) -> String {
    if !target_seen {
        return format!(
            "# Callers of \"{target}\" in {scope_disp} — no call sites found\n\n\
             The name \"{target}\" does not appear anywhere in scope. \
             Check the spelling, or widen scope if you expected hits outside this directory.",
            scope_disp = scope.display()
        );
    }
    // Only mention glob-driven test exclusion when a glob was actually used.
    // Otherwise the line implies a filter that the caller didn't apply, which
    // would mislead an agent reasoning about what tilth searched.
    let glob_hint = if glob.is_some() {
        "\n  • test files (if `glob` excluded them)"
    } else {
        ""
    };
    format!(
        "# Callers of \"{target}\" in {scope_disp} — no direct call sites found\n\n\
         \"{target}\" appears in the codebase but has no syntactic call sites. \
         tilth detects only direct, by-name calls; this symbol may still be reachable via:\n\
         \n  • interface / trait dispatch (Rust `dyn Trait`, Go interface, Java/Kotlin abstract method)\
         \n  • reflection or dynamic dispatch (`getattr`, `Method::invoke`, `eval`)\
         \n  • framework registration (HTTP routes, JSON-RPC, plugin systems, decorators)\
         \n  • function values stored in maps, structs, or passed as callbacks{glob_hint}\n\
         \nVerify with `tilth_search \"{target}\"` to see how it's referenced before assuming dead code.",
        scope_disp = scope.display()
    )
}

/// Simple ranking: context file first, then by path length (proximity heuristic).
fn rank_callers(callers: &mut [CallerMatch], scope: &Path, context: Option<&Path>) {
    callers.sort_by(|a, b| {
        // Context file wins
        if let Some(ctx) = context {
            match (a.path == ctx, b.path == ctx) {
                (true, false) => return std::cmp::Ordering::Less,
                (false, true) => return std::cmp::Ordering::Greater,
                _ => {}
            }
        }

        // Shorter paths (more similar to scope) rank higher
        let a_rel = a.path.strip_prefix(scope).unwrap_or(&a.path);
        let b_rel = b.path.strip_prefix(scope).unwrap_or(&b.path);
        a_rel
            .components()
            .count()
            .cmp(&b_rel.components().count())
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_matches_reuse_original_file_content_arc() {
        let source = Arc::new(
            "fn callee() {}\n\nfn caller() {\n    callee();\n    callee();\n}\n".to_string(),
        );
        let targets = HashSet::from(["callee".to_string()]);
        let lang = crate::types::Lang::Rust;
        let ts_lang = outline_language(lang).expect("rust grammar should be available");

        let matches = find_callers_treesitter_batch(
            Path::new("sample.rs"),
            &targets,
            &ts_lang,
            &source,
            lang,
        );

        let mut actual = Vec::new();
        for (target, caller) in &matches {
            actual.push((target.as_str(), caller.line, caller.call_text.as_str()));
        }
        actual.sort_by_key(|&(_, line, _)| line);
        assert_eq!(
            actual,
            vec![("callee", 4, "callee();"), ("callee", 5, "callee();")]
        );
        for (_, caller) in &matches {
            assert!(
                Arc::ptr_eq(&caller.content, &source),
                "caller content should reuse the Arc created for the file"
            );
        }
    }

    /// MED finding from PR review: the batch walk's early-quit budget is a
    /// walk-wide raw-match count shared by every target passed to
    /// `find_callers_batch` — a single target's budget therefore starves
    /// later targets in a multi-target search. `scaled_batch_quit` is the
    /// pure scaling function `search_callers_multi_expanded` uses to size
    /// the walk's budget by target count instead of reusing the unscaled
    /// single-target constant. This asserts the scaling directly (rather
    /// than only via an integration test against the parallel walker, whose
    /// starvation is real but not reliably reproducible in a small,
    /// deterministic unit test — see
    /// `callers_multi_target_later_target_not_starved_by_hit_rich_earlier_target`
    /// in `src/mcp/tools/search.rs` for that scenario-level guard).
    #[test]
    fn scaled_batch_quit_multiplies_by_target_count() {
        assert_eq!(scaled_batch_quit(BATCH_EARLY_QUIT, 1), BATCH_EARLY_QUIT);
        assert_eq!(
            scaled_batch_quit(BATCH_EARLY_QUIT, 2),
            BATCH_EARLY_QUIT * 2,
            "2 targets must not share a single target's budget"
        );
        assert_eq!(scaled_batch_quit(BATCH_EARLY_QUIT, 5), BATCH_EARLY_QUIT * 5);
    }

    /// `n_targets = 0` cannot happen through the dispatch layer (`tool_search`
    /// rejects an empty query before reaching `search_callers_multi_expanded`),
    /// but the scaling function must stay total rather than dividing by zero
    /// or returning a zero budget that would make every walk quit instantly.
    #[test]
    fn scaled_batch_quit_treats_zero_targets_as_one() {
        assert_eq!(scaled_batch_quit(BATCH_EARLY_QUIT, 0), BATCH_EARLY_QUIT);
    }

    #[test]
    fn no_callers_message_for_unseen_symbol_says_typo_or_scope() {
        let msg = no_callers_message("doesNotExist", Path::new("/repo"), false, None);
        assert!(msg.contains("does not appear anywhere in scope"));
        assert!(msg.contains("Check the spelling"));
        // Must NOT include the indirect-dispatch hint — that would mislead.
        assert!(!msg.contains("interface"));
        assert!(!msg.contains("reflection"));
    }

    #[test]
    fn no_callers_message_for_seen_symbol_lists_indirection_modes() {
        let msg = no_callers_message("Foo", Path::new("/repo"), true, None);
        assert!(msg.contains("appears in the codebase"));
        assert!(msg.contains("interface"));
        assert!(msg.contains("reflection"));
        assert!(msg.contains("framework registration"));
        assert!(msg.contains("Verify with `tilth_search"));
        // Must NOT pretend the symbol is missing — different signal than typo case.
        assert!(!msg.contains("does not appear"));
    }

    /// The "test files (if glob excluded them)" hint is only meaningful when
    /// the caller actually used a glob. Without a glob it would mislead an
    /// agent into thinking tilth filtered something it did not.
    #[test]
    fn no_callers_message_omits_glob_hint_when_no_glob() {
        let msg = no_callers_message("Foo", Path::new("/repo"), true, None);
        assert!(
            !msg.contains("test files"),
            "glob-driven hint must not appear when glob is None: {msg}"
        );
    }

    #[test]
    fn no_callers_message_includes_glob_hint_when_glob_set() {
        let msg = no_callers_message("Foo", Path::new("/repo"), true, Some("*.rs"));
        assert!(
            msg.contains("test files"),
            "glob-driven hint should appear when glob is Some: {msg}"
        );
    }

    #[test]
    fn footer_count_uses_pre_truncation_caller_set() {
        // >MAX_MATCHES hop-1 call sites but <= IMPACT_FANOUT_THRESHOLD unique
        // callers: the "N functions affected across 2 hops" footer must use the
        // pre-truncation distinct-caller count (all_caller_names), not the
        // post-truncation rebuild. With MAX_MATCHES == 10: 8 funcs x 2 call sites
        // = 16 sites; truncate(MAX_MATCHES) keeps ~5 funcs (old undercount),
        // pre-truncation set is 8; +1 hop-2 = 9.
        let dir = tempfile::tempdir().unwrap();
        let bloom = crate::index::bloom::BloomFilterCache::new();
        for i in 0..8usize {
            let content = format!(
                "fn target_fn() {{}}\
                \nfn caller_a_{i}() {{ target_fn(); target_fn(); }}\
                \n"
            );
            std::fs::write(dir.path().join(format!("f{i}.rs")), content).unwrap();
        }
        std::fs::write(
            dir.path().join("hop2.rs"),
            "fn hop2_fn() { caller_a_0(); }\n",
        )
        .unwrap();
        let result =
            search_callers_expanded("target_fn", dir.path(), &bloom, 0, None, None, false).unwrap();
        let footer_line = result
            .lines()
            .find(|l| l.contains("functions affected across 2 hops"))
            .unwrap_or_else(|| panic!("footer line missing from output:\n{result}"));
        let reported: usize = footer_line
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap_or_else(|_| panic!("footer count not a number: {footer_line}"));
        assert_eq!(
            reported, 9,
            "footer reported {reported} but expected exactly 9 (8 hop-1 + 1 hop-2): {footer_line}"
        );
    }
}
