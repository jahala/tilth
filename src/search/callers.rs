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
    content: &str,
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

    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };

    let content_bytes = content.as_bytes();
    let lines: Vec<&str> = content.lines().collect();

    // One Arc per file — all call sites share the same allocation.
    let shared_content: Arc<String> = Arc::new(content.to_string());

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
                        content: Arc::clone(&shared_content),
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

    // Format the output
    let mut output = format!(
        "# Callers of \"{}\" in {} — {} call site{}\n",
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
        let _ = writeln!(output, "→ {}", caller.call_text);

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
                        "► "
                    } else {
                        "  "
                    };
                    let _ = writeln!(output, "{prefix}{line_num:4} │ {line}");
                }

                output.push_str("```\n");
            }
        }
    }

    // ── Adaptive 2nd-hop impact analysis ──
    // Use all_caller_names (pre-truncation) for the fan-out threshold check,
    // but search for callers of the full set to capture transitive impact.
    if !all_caller_names.is_empty() && all_caller_names.len() <= IMPACT_FANOUT_THRESHOLD {
        if let Ok(hop2) = find_callers_batch(&all_caller_names, scope, bloom, glob, batch_quit) {
            // Filter out hop-1 matches (same file+line = same call site)
            let hop1_locations: HashSet<(PathBuf, u32)> = sorted_callers
                .iter()
                .map(|c| (c.path.clone(), c.line))
                .collect();

            let hop2_filtered: Vec<_> = hop2
                .into_iter()
                .filter(|(_, m)| !hop1_locations.contains(&(m.path.clone(), m.line)))
                .collect();

            if !hop2_filtered.is_empty() {
                output.push_str("\n── impact (2nd hop) ──\n");

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
                        "  {:<20} {}:{}  \u{2192} {}",
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

                let _ = writeln!(
                    output,
                    "\n{} functions affected across 2 hops.",
                    all_caller_names.len() + unique_total
                );
            }
        }
    }

    let tokens = crate::types::estimate_tokens(output.len() as u64);
    let _ = write!(
        output,
        "\n\n({} tokens)",
        crate::search::format_token_count(tokens)
    );
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
    /// Regression test: when there are more than MAX_MATCHES (10) hop-1 call
    /// sites but still <= IMPACT_FANOUT_THRESHOLD unique callers, the footer
    /// "N functions affected across 2 hops" must use the pre-truncation unique
    /// count, not the post-truncation count.
    ///
    /// Setup: 8 unique functions, each calling `target_fn` twice = 16 call
    /// sites. Truncation to MAX_MATCHES=10 only keeps the first ~5 functions,
    /// dropping functions 6-8. The old code rebuilt the hop-1 set from
    /// sorted_callers AFTER truncation and undercounted. The fix uses
    /// all_caller_names (pre-truncation) which always holds 8.
    #[test]
    fn footer_count_uses_pre_truncation_caller_set() {
        let dir = tempfile::tempdir().unwrap();
        let bloom = crate::index::bloom::BloomFilterCache::new();

        // 8 files: each declares one function that calls `target_fn` twice.
        // Total: 16 call sites from 8 unique caller names.
        // One hop-2 file calls caller_a_0 so the 2nd-hop block fires.
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

        // hop-1 = 8 (all_caller_names, pre-truncation); hop-2 = 1 (hop2_fn → caller_a_0).
        assert_eq!(
            reported, 9,
            "footer reported {reported} but expected exactly 9 (8 hop-1 + 1 hop-2); \
             old post-truncation rebuild would undercount: {footer_line}"
        );
    }
}
