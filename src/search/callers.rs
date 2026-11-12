//! Caller search output: ranks the call sites `tilth_core` finds, renders one
//! bucket per target, and adds the second-hop impact block.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::format::noun;
use tilth_core::index::bloom::BloomFilterCache;
use tilth_core::search::callers::{find_callers_and_owners, find_callers_batch, CallerMatch};
use tilth_core::search::walk::Walk;
use tilth_core::TilthError;

const MAX_MATCHES: usize = 10;
/// Max unique caller functions to trace for 2nd hop. Above this = wide fan-out, skip.
const IMPACT_FANOUT_THRESHOLD: usize = 10;
/// Max 2nd-hop results to display.
const IMPACT_MAX_RESULTS: usize = 15;
/// Match-count cap when `--full` is set. Mirrors the symbol/content search caps.
const FULL_MAX_MATCHES: usize = 100;

/// One target's call sites over the whole walk.
#[derive(Default)]
struct TargetSites {
    /// Every call site, counted.
    total: usize,
    /// Every distinct calling function, `<top-level>` left out.
    caller_names: HashSet<String>,
    /// The first sites in `rank_callers` order.
    kept: Vec<CallerMatch>,
}

/// The call sites of each of `targets`, counted in full, with each target's
/// first `keep` sites in `rank_callers` order kept.
///
/// Each file offers its first `keep` sites per target by line, and they are
/// merged into the running first `keep`. That loses nothing: `rank_callers`
/// compares two sites of one file by line alone, so any site in the overall
/// first `keep` is in its own file's first `keep`. The order is total across
/// files, since it ends on path and line, so the result does not depend on
/// the order files arrive in. Only `keep` sites per target are held at once,
/// which keeps memory flat on a large tree: each site holds its file's text.
/// Also returns the code files skipped for size that hold a target.
fn collect_sites(
    targets: &HashSet<String>,
    walk: Walk<'_>,
    bloom: &BloomFilterCache,
    context: Option<&Path>,
    keep: usize,
) -> Result<(HashMap<String, TargetSites>, Vec<PathBuf>), TilthError> {
    let merged: Mutex<HashMap<String, TargetSites>> = Mutex::new(HashMap::new());
    let walked = find_callers_batch(targets, walk, bloom, None, |sites| {
        let mut by_target: HashMap<String, TargetSites> = HashMap::new();
        for (target, m) in sites {
            let entry = by_target.entry(target).or_default();
            entry.total += 1;
            if m.calling_function != "<top-level>" {
                entry.caller_names.insert(m.calling_function.clone());
            }
            if entry.kept.len() < keep {
                entry.kept.push(m);
            }
        }
        let mut merged = merged
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (target, file) in by_target {
            let entry = merged.entry(target).or_default();
            entry.total += file.total;
            entry.caller_names.extend(file.caller_names);
            entry.kept.extend(file.kept);
            rank_callers(&mut entry.kept, walk.scope, context);
            entry.kept.truncate(keep);
        }
    })?;
    let merged = merged
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Ok((merged, walked.skipped_large))
}

/// Scan the files `walk` visits for the literal `target` byte sequence.
/// Used by the single-symbol `search_callers_expanded` path to distinguish
/// "typo, doesn't exist" from "real symbol with no direct callers" (indirect
/// dispatch, dead code, framework registration, …) when the caller walk
/// returned zero matches.
fn target_seen_in_scope(target: &str, walk: Walk<'_>) -> bool {
    let Ok(walker) = tilth_core::search::walk::walker(walk) else {
        return false;
    };
    let needle = target.as_bytes();
    let seen = AtomicBool::new(false);

    walker.run(|| {
        let seen = &seen;
        // One finder per walker thread, its buffers reused for every file.
        let mut finder = tilth_core::source::StreamFinder::new(needle);
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
            // Streamed through the finder's buffer, so a multi-gigabyte file
            // costs one chunk of memory, and the scan stops at the first
            // match. The open never blocks, so a pipe swapped in for the
            // file after the walk listed it reads as empty. A file that
            // cannot be read counts as not naming the target, as before.
            let Ok(Some(mut file)) = tilth_core::source::OpenFile::open(entry.path()) else {
                return ignore::WalkState::Continue;
            };
            if file
                .reader()
                .and_then(|reader| finder.contains(reader))
                .unwrap_or(false)
            {
                seen.store(true, Ordering::Relaxed);
                return ignore::WalkState::Quit;
            }
            ignore::WalkState::Continue
        })
    });

    seen.load(Ordering::Relaxed)
}

/// Format and rank caller search results with optional expand.
pub fn search_callers_expanded(
    target: &str,
    walk: Walk<'_>,
    bloom: &BloomFilterCache,
    expand: usize,
    context: Option<&Path>,
    full: bool,
) -> Result<String, TilthError> {
    let scope = walk.scope;
    let max_matches = if full { FULL_MAX_MATCHES } else { MAX_MATCHES };
    let name = called_name(target);
    let single: HashSet<String> = std::iter::once(name.to_string()).collect();
    let (mut by_target, skipped_large) = collect_sites(&single, walk, bloom, context, max_matches)?;
    let sites = by_target.remove(name).unwrap_or_default();
    let held = crate::search::names_phrase([name]);
    let skipped_note = crate::search::skipped_files_note(&skipped_large, &held, scope)
        .map(|sentence| format!("\n\n> {sentence}"))
        .unwrap_or_default();

    if sites.total == 0 {
        let target_seen = target_seen_in_scope(name, walk);
        let mut message = no_callers_message(target, scope, target_seen, walk.glob);
        message.push_str(&skipped_note);
        return Ok(message);
    }

    let mut output = String::new();
    write_caller_bucket(&mut output, target, scope, sites.total, &sites.kept, expand);
    write_second_hop_impact(&mut output, &sites.caller_names, &sites.kept, walk, bloom);

    output.push_str(&skipped_note);

    let tokens = crate::types::estimate_tokens(output.len() as u64);
    let _ = write!(
        output,
        "\n\n({} tokens)",
        crate::search::format_token_count(tokens)
    );
    Ok(output)
}

/// The name a call site of `target` is written with. A caller label names
/// its owner (`Engine.handle`), but a call names the method alone
/// (`e.handle()`), so a qualified target is looked up by its name.
fn called_name(target: &str) -> &str {
    tilth_core::search::scope::split_qualified(target).1
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
    let shown = if sorted_callers.len() < total {
        format!("{} of ", sorted_callers.len())
    } else {
        String::new()
    };
    let _ = writeln!(
        output,
        "# Callers of \"{}\" in {} — {shown}{} {}",
        target,
        scope.display(),
        total,
        noun(total, "call site", "call sites")
    );
    let name = called_name(target);
    if name != target {
        let _ = writeln!(
            output,
            "> Calls are matched by the name `{name}`; the receiver's type is not checked."
        );
    }

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

    let hidden = total.saturating_sub(sorted_callers.len());
    if hidden > 0 {
        let _ = writeln!(
            output,
            "\n... and {hidden} more {}. Narrow with scope.",
            noun(hidden, "call site", "call sites")
        );
    }
}

/// One file's part of the second hop. Which method names several types
/// define is known only once the walk ends, so a file keeps what either
/// answer needs: the first call of each name by each function, and its
/// call sites per name.
struct Hop2File {
    path: PathBuf,
    /// (name called, calling function, line) for the first call of each
    /// name by each function, in line order.
    firsts: Vec<(String, String, u32)>,
    /// Call sites of each name.
    sites: HashMap<String, usize>,
    /// (name, owner) for each owned definition of a method name.
    owners: Vec<(String, String)>,
}

/// A function the second hop lists: where it first calls a name, and the
/// name.
struct Hop2Site {
    via: String,
    function: String,
    path: PathBuf,
    line: u32,
}

/// Adaptive 2nd-hop impact analysis, shared by single- and multi-target
/// callers search (extracted so multi-target reuses this exact block per
/// target bucket instead of re-implementing it — PR #138 review HIGH
/// finding: the multi-target path originally omitted this entirely).
///
/// `all_caller_names` must be the target's unique direct-caller names
/// collected BEFORE `sorted_callers` truncation, so the fan-out threshold
/// check reflects the true hop-1 breadth rather than the display-capped one.
#[expect(
    clippy::too_many_lines,
    reason = "renderers become functions of a result model in #311"
)]
fn write_second_hop_impact(
    output: &mut String,
    all_caller_names: &HashSet<String>,
    sorted_callers: &[CallerMatch],
    walk: Walk<'_>,
    bloom: &BloomFilterCache,
) {
    if all_caller_names.is_empty() || all_caller_names.len() > IMPACT_FANOUT_THRESHOLD {
        return;
    }

    // Filter out hop-1 matches (same file+line = same call site)
    let hop1_locations: HashSet<(PathBuf, u32)> = sorted_callers
        .iter()
        .map(|c| (c.path.clone(), c.line))
        .collect();

    // A label names its owner (`Service.run`); a call site names the method
    // alone (`s.run()`), so the second hop looks for the bare names. A site
    // of a method name that several types define could reach any of them,
    // so those sites are counted apart instead of listed as affected. The
    // walk that finds the sites also finds the types that define each
    // method name, from the same parse.
    let mut hop2_targets: HashSet<String> = HashSet::new();
    let mut method_names: HashSet<String> = HashSet::new();
    for label in all_caller_names {
        let (owner, name) = tilth_core::search::scope::split_qualified(label);
        hop2_targets.insert(name.to_string());
        if owner.is_some() {
            method_names.insert(name.to_string());
        }
    }

    let walked = find_callers_and_owners(
        &hop2_targets,
        &method_names,
        walk,
        bloom,
        |sites, owners| {
            let mut file = Hop2File {
                path: sites
                    .first()
                    .map(|(_, m)| m.path.clone())
                    .unwrap_or_default(),
                firsts: Vec::new(),
                sites: HashMap::new(),
                owners,
            };
            let mut seen: HashSet<(String, String)> = HashSet::new();
            for (via, m) in sites {
                if hop1_locations.contains(&(m.path.clone(), m.line)) {
                    continue;
                }
                *file.sites.entry(via.clone()).or_default() += 1;
                if seen.insert((via.clone(), m.calling_function.clone())) {
                    file.firsts.push((via, m.calling_function, m.line));
                }
            }
            file
        },
    );
    let Ok(walked) = walked else {
        return;
    };

    let mut types: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (name, owner) in walked.per_file.iter().flat_map(|f| &f.owners) {
        types.entry(name).or_default().insert(owner);
    }
    let ambiguous: HashMap<&str, usize> = types
        .into_iter()
        .filter(|(_, types)| types.len() > 1)
        .map(|(name, types)| (name, types.len()))
        .collect();

    // Per file: each calling function once, at its first call by line of a
    // name not ambiguous. The listing is the first `IMPACT_MAX_RESULTS` of
    // those by depth, path and line; the count of distinct functions is kept
    // in full for the "... and N more" line and the affected total.
    let mut unique_total = 0;
    let mut listed: Vec<Hop2Site> = Vec::new();
    let mut unattributed: HashMap<&str, usize> = HashMap::new();
    let mut by_method_name = false;
    for file in &walked.per_file {
        for (via, &n) in &file.sites {
            if ambiguous.contains_key(via.as_str()) {
                *unattributed.entry(via).or_default() += n;
            } else {
                by_method_name |= method_names.contains(via);
            }
        }
        let mut functions: HashSet<&str> = HashSet::new();
        for (via, function, line) in &file.firsts {
            if !ambiguous.contains_key(via.as_str()) && functions.insert(function) {
                listed.push(Hop2Site {
                    via: via.clone(),
                    function: function.clone(),
                    path: file.path.clone(),
                    line: *line,
                });
            }
        }
        unique_total += functions.len();
    }
    listed.sort_by(|a, b| {
        path_depth(&a.path, walk.scope)
            .cmp(&path_depth(&b.path, walk.scope))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
    listed.truncate(IMPACT_MAX_RESULTS);
    if unique_total == 0 && unattributed.is_empty() {
        return;
    }

    output.push_str("\n-- impact (2nd hop) --\n");

    for site in &listed {
        let rel_path = site
            .path
            .strip_prefix(walk.scope)
            .unwrap_or(&site.path)
            .display();
        let _ = writeln!(
            output,
            "  {:<20} {}:{}  -> {}",
            site.function, rel_path, site.line, site.via
        );
    }

    if unique_total > IMPACT_MAX_RESULTS {
        let _ = writeln!(
            output,
            "  ... and {} more",
            unique_total - IMPACT_MAX_RESULTS
        );
    }

    let mut unattributed: Vec<(&str, usize, usize)> = unattributed
        .into_iter()
        .map(|(name, sites)| (name, sites, ambiguous[name]))
        .collect();
    unattributed.sort_unstable();
    for (name, sites, types) in unattributed {
        let _ = writeln!(
            output,
            "  {name}: {sites} {}, not attributed ({types} types define it)",
            noun(sites, "call site", "call sites"),
        );
    }

    // Affected-count formula matches main's evolution: pre-truncation
    // hop-1 caller-name count + deduplicated hop-2 unique_total (NOT the
    // post-truncation display count, which under-reports once the
    // `IMPACT_MAX_RESULTS` cap stops the listing while more unique callers
    // still exist beyond the cap).
    let affected = all_caller_names.len() + unique_total;
    let _ = writeln!(
        output,
        "\n{affected} {} affected across 2 hops{}.",
        noun(affected, "function", "functions"),
        if by_method_name {
            " (method calls matched by name)"
        } else {
            ""
        }
    );
}

/// Multi-target caller search: find call sites of 2..=5 symbols in a single
/// walk via `find_callers_batch`, then render one labeled section per target.
/// Mirrors `search_multi_symbol_expanded` for the `kind=callers` comma path.
///
/// Each target's bucket renders via the same `write_caller_bucket` +
/// `write_second_hop_impact` helpers the single-target path uses, so a
/// bucket here is byte-identical to what a lone `search_callers_expanded`
/// call for that target would produce (PR #138 review: HIGH — 2nd-hop parity;
/// MED — header shape parity). The walk runs to the end and counts each
/// target on its own, so a hit-rich target cannot starve a rarer one.
pub fn search_callers_multi_expanded(
    targets: &[&str],
    walk: Walk<'_>,
    bloom: &BloomFilterCache,
    expand: usize,
    context: Option<&Path>,
    full: bool,
) -> Result<String, TilthError> {
    let scope = walk.scope;
    let max_matches = if full { FULL_MAX_MATCHES } else { MAX_MATCHES };

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

    // The bare names the walk looks for, in target order, each once.
    let mut names: Vec<&str> = Vec::new();
    for target in &ordered {
        let name = called_name(target);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let target_set: HashSet<String> = names.iter().map(|n| (*n).to_string()).collect();
    let (by_target, skipped_large) = collect_sites(&target_set, walk, bloom, context, max_matches)?;

    // Render in the caller-supplied target order so output is deterministic.
    // Two targets can share a name (`A.run,B.run`), so each reads its sites.
    let none = TargetSites::default();
    let mut output = String::new();
    for target in &ordered {
        let name = called_name(target);
        let sites = by_target.get(name).unwrap_or(&none);

        if sites.total == 0 {
            let target_seen = target_seen_in_scope(name, walk);
            output.push_str(&no_callers_message(target, scope, target_seen, walk.glob));
            output.push_str("\n\n");
            continue;
        }

        write_caller_bucket(&mut output, target, scope, sites.total, &sites.kept, expand);
        write_second_hop_impact(&mut output, &sites.caller_names, &sites.kept, walk, bloom);
        output.push('\n');
    }

    let held = crate::search::names_phrase(names.iter().copied());
    if let Some(sentence) = crate::search::skipped_files_note(&skipped_large, &held, scope) {
        let _ = writeln!(output, "> {sentence}");
    }

    let tokens = crate::types::estimate_tokens(output.len() as u64);
    // Single leading '\n' (not single-target's '\n\n'): the per-bucket loop
    // above already ends each target's section with its own `output.push('\n')`,
    // so a second blank line here would double up before the token count.
    let _ = write!(
        output,
        "\n({} tokens)",
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

/// Simple ranking: context file first, then by path length (proximity
/// heuristic), then by path and line. Stable, so two calls on one line keep
/// their order.
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
        path_depth(&a.path, scope)
            .cmp(&path_depth(&b.path, scope))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
}

/// How many components `path` has below `scope`.
fn path_depth(path: &Path, scope: &Path) -> usize {
    path.strip_prefix(scope)
        .unwrap_or(path)
        .components()
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilth_core::search::walk::WalkConfig;

    /// The caller walk quit on a shared counter at 50 sites, so the header
    /// total was wherever the threads stopped: "175 call sites" for 625, and
    /// the sites listed changed from run to run.
    #[test]
    fn caller_totals_are_exact_and_identical_across_runs() {
        let dir = tempfile::tempdir().unwrap();
        let bloom = BloomFilterCache::new();
        std::fs::write(dir.path().join("def.rs"), "pub fn hot() {}\n").unwrap();
        for f in 0..25 {
            let mut body = String::new();
            for i in 0..25 {
                let _ = writeln!(body, "fn caller_{f}_{i}() {{ hot(); }}");
            }
            std::fs::write(dir.path().join(format!("f{f:02}.rs")), body).unwrap();
        }
        let first = search_callers_expanded(
            "hot",
            Walk::new(dir.path(), WalkConfig::default()),
            &bloom,
            0,
            None,
            false,
        )
        .unwrap();
        assert!(
            first.contains("— 10 of 625 call sites"),
            "header must give the exact total:\n{first}"
        );
        for _ in 0..4 {
            let again = search_callers_expanded(
                "hot",
                Walk::new(dir.path(), WalkConfig::default()),
                &bloom,
                0,
                None,
                false,
            )
            .unwrap();
            assert_eq!(again, first, "same output every run");
        }
        let full = search_callers_expanded(
            "hot",
            Walk::new(dir.path(), WalkConfig::default()),
            &bloom,
            0,
            None,
            true,
        )
        .unwrap();
        assert!(full.contains("— 100 of 625 call sites"), "{full}");
    }

    /// A list cut short must say so: 12 call sites with 10 shown said
    /// nothing about the other two.
    #[test]
    fn a_capped_caller_list_says_how_many_are_not_shown() {
        let dir = tempfile::tempdir().unwrap();
        let bloom = BloomFilterCache::new();
        let mut body = String::from("pub fn target_fn() {}\n");
        for i in 0..12 {
            let _ = writeln!(body, "fn c{i}() {{ target_fn(); }}");
        }
        std::fs::write(dir.path().join("lib.rs"), body).unwrap();
        let out = search_callers_expanded(
            "target_fn",
            Walk::new(dir.path(), WalkConfig::default()),
            &bloom,
            0,
            None,
            false,
        )
        .unwrap();
        assert!(out.contains("— 10 of 12 call sites"), "{out}");
        assert!(out.contains("... and 2 more call sites."), "{out}");
        assert_eq!(out.matches("\n## ").count(), 10);
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
    /// A call inside a macro's arguments is parsed as raw tokens in a
    /// `token_tree`, not a `call_expression`, so the call-expression patterns
    /// alone never saw it — `write!(out, "{}", alpha(1))` reported no caller at
    /// all. The negative fixture is the shape the token-tree pattern matches
    /// too loosely and `callee_filter` has to reject.
    #[test]
    fn callers_sees_calls_inside_macro_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let bloom = BloomFilterCache::new();

        std::fs::write(
            dir.path().join("a.rs"),
            "fn alpha(_n: u32) {}\n\
             fn macro_arg_caller(out: &mut String) { let _ = write!(out, \"{}\", alpha(1)); }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("b.rs"),
            "fn tuple_not_a_call() { assert_eq!(alpha, (1, 2)); }\n",
        )
        .unwrap();

        let result = search_callers_expanded(
            "alpha",
            Walk::new(dir.path(), WalkConfig::default()),
            &bloom,
            0,
            None,
            false,
        )
        .unwrap();

        assert!(
            result.contains("[caller: macro_arg_caller]"),
            "call site inside macro arguments not reported:\n{result}"
        );
        assert!(
            !result.contains("tuple_not_a_call"),
            "a bare token before a tuple must not count as a call:\n{result}"
        );
    }

    /// `call_text` shows the call's source line. When the capture's parent
    /// spans lines there is no one line that is the call, so it falls back to
    /// the callee's own line — in every language, not only Rust. A macro's
    /// token tree nearly always spans lines, so without this the call sites the
    /// token-tree pattern finds would render as a bare echo of the query.
    #[test]
    fn call_text_falls_back_to_the_callee_line_across_languages() {
        let dir = tempfile::tempdir().unwrap();
        let bloom = BloomFilterCache::new();

        std::fs::write(
            dir.path().join("wrapped.py"),
            "def target(n):\n    return n\n\ndef py_multi():\n    return target(\n        1,\n    )\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("wrapped.rs"),
            "fn target(n: u32) -> u32 { n }\n\
             fn rs_macro(out: &mut String) {\n\
             \x20   let _ = write!(\n\
             \x20       out,\n\
             \x20       \"{}\",\n\
             \x20       target(1),\n\
             \x20   );\n\
             }\n",
        )
        .unwrap();

        let result = search_callers_expanded(
            "target",
            Walk::new(dir.path(), WalkConfig::default()),
            &bloom,
            0,
            None,
            false,
        )
        .unwrap();

        assert!(
            result.contains("-> return target("),
            "a wrapped Python call should render its own line:\n{result}"
        );
        assert!(
            result.contains("-> target(1),"),
            "a call inside a multi-line macro should render its own line:\n{result}"
        );
    }

    /// Regression test: when there are more than `MAX_MATCHES` (10) hop-1 call
    /// sites but still <= `IMPACT_FANOUT_THRESHOLD` unique callers, the footer
    /// "N functions affected across 2 hops" must use the pre-truncation unique
    /// count, not the post-truncation count.
    ///
    /// Setup: 8 unique functions, each calling `target_fn` twice = 16 call
    /// sites. Truncation to `MAX_MATCHES=10` only keeps the first ~5 functions,
    /// dropping functions 6-8. The old code rebuilt the hop-1 set from
    /// `sorted_callers` AFTER truncation and undercounted. The fix uses
    /// `all_caller_names` (pre-truncation) which always holds 8.
    #[test]
    fn footer_count_uses_pre_truncation_caller_set() {
        let dir = tempfile::tempdir().unwrap();
        let bloom = BloomFilterCache::new();

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

        let result = search_callers_expanded(
            "target_fn",
            Walk::new(dir.path(), WalkConfig::default()),
            &bloom,
            0,
            None,
            false,
        )
        .unwrap();

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

    /// The second hop finds the callers of each first-hop caller by its own
    /// name. A method's label carries its owner (`Service.run`); the call
    /// sites of that method name only `run`.
    #[test]
    fn second_hop_reaches_the_callers_of_a_method() {
        let dir = tempfile::tempdir().unwrap();
        let bloom = BloomFilterCache::new();
        std::fs::write(
            dir.path().join("service.ts"),
            "function target() { return 1; }\n\
             class Service {\n  run() { return target(); }\n}\n\
             function outer() {\n  const result = target();\n  return result;\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("app.ts"),
            "function app(s: Service) { return s.run(); }\n",
        )
        .unwrap();

        let result = search_callers_expanded(
            "target",
            Walk::new(dir.path(), WalkConfig::default()),
            &bloom,
            0,
            None,
            false,
        )
        .unwrap();

        assert!(result.contains("[caller: Service.run]"), "{result}");
        assert!(result.contains("[caller: outer]"), "{result}");
        let impact = result
            .split("-- impact (2nd hop) --")
            .nth(1)
            .unwrap_or_else(|| panic!("no 2nd-hop block:\n{result}"));
        assert!(impact.contains("app.ts:1"), "{result}");
    }

    /// A call names a method without its type, so when several types define
    /// that name the second hop cannot tell whose method a site reaches.
    /// Every class whose `__init__` calls `super().__init__()` would be
    /// listed as affected. The sites are counted on one line instead, and
    /// the real chain keeps its place.
    #[test]
    fn second_hop_counts_a_method_several_types_define_apart() {
        let dir = tempfile::tempdir().unwrap();
        let bloom = BloomFilterCache::new();
        std::fs::write(
            dir.path().join("routes.py"),
            "def target():\n    return 1\n\n\
             class Route:\n    def __init__(self):\n        target()\n\n\
             def helper():\n    return target()\n\n\
             def app():\n    return helper()\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("models.py"),
            "class Base:\n    def __init__(self):\n        pass\n\n\
             class First(Base):\n    def __init__(self):\n        super().__init__()\n\n\
             class Second(Base):\n    def __init__(self):\n        super().__init__()\n",
        )
        .unwrap();

        let result = search_callers_expanded(
            "target",
            Walk::new(dir.path(), WalkConfig::default()),
            &bloom,
            0,
            None,
            false,
        )
        .unwrap();

        assert!(result.contains("[caller: Route.__init__]"), "{result}");
        let impact = result
            .split("-- impact (2nd hop) --")
            .nth(1)
            .unwrap_or_else(|| panic!("no 2nd-hop block:\n{result}"));
        assert!(impact.contains("app "), "{result}");
        assert!(impact.contains("-> helper"), "{result}");
        assert!(!impact.contains("First.__init__"), "{result}");
        assert!(!impact.contains("Second.__init__"), "{result}");
        assert!(
            impact.contains("__init__: 2 call sites, not attributed (4 types define it)"),
            "{result}"
        );
        assert!(
            impact.contains("3 functions affected across 2 hops."),
            "{result}"
        );
    }

    /// A caller label names its owner (`Engine.handle`), and passing one back
    /// to callers found nothing: no call site is written that way. A
    /// qualified target is looked up by its name, and the header says so.
    #[test]
    fn a_qualified_target_finds_the_calls_of_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let bloom = BloomFilterCache::new();
        std::fs::write(
            dir.path().join("engine.go"),
            "package gin\n\ntype Engine struct{}\n\nfunc (e *Engine) handle() {}\n\n\
             func (e *Engine) ServeHTTP() {\n\te.handle()\n}\n",
        )
        .unwrap();
        let bare = search_callers_expanded(
            "handle",
            Walk::new(dir.path(), WalkConfig::default()),
            &bloom,
            0,
            None,
            false,
        )
        .unwrap();
        assert!(bare.contains("[caller: Engine.ServeHTTP]"), "{bare}");
        for target in ["Engine.handle", "Engine::handle"] {
            let out = search_callers_expanded(
                target,
                Walk::new(dir.path(), WalkConfig::default()),
                &bloom,
                0,
                None,
                false,
            )
            .unwrap();
            assert!(out.contains(&format!("# Callers of \"{target}\"")), "{out}");
            assert!(out.contains("— 1 call site"), "{out}");
            assert!(
                out.contains("engine.go:8 [caller: Engine.ServeHTTP]"),
                "{out}"
            );
            assert!(out.contains("matched by the name `handle`"), "{out}");
        }
        let multi = search_callers_multi_expanded(
            &["Engine.handle", "ServeHTTP"],
            Walk::new(dir.path(), WalkConfig::default()),
            &bloom,
            0,
            None,
            false,
        )
        .unwrap();
        assert!(
            multi.contains("# Callers of \"Engine.handle\" in"),
            "{multi}"
        );
        assert!(
            multi.contains("engine.go:8 [caller: Engine.ServeHTTP]"),
            "{multi}"
        );
    }

    /// The second hop traces a method by its name alone. When one type in
    /// the repository defines the name, a call on any other receiver (a
    /// `dict.get` for `Cache.get`) counts as reaching it, so the affected
    /// total says the method calls were matched by name.
    #[test]
    fn a_second_hop_through_a_method_says_it_matched_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let bloom = BloomFilterCache::new();
        std::fs::write(
            dir.path().join("cache.py"),
            "def fetch(key):\n    return key\n\n\nclass Cache:\n    def get(self, key):\n        return fetch(key)\n\n\n\
             def lookup(d):\n    return d.get(\"k\")\n",
        )
        .unwrap();
        let out = search_callers_expanded(
            "fetch",
            Walk::new(dir.path(), WalkConfig::default()),
            &bloom,
            0,
            None,
            false,
        )
        .unwrap();
        assert!(out.contains("[caller: Cache.get]"), "{out}");
        assert!(out.contains("lookup"), "{out}");
        assert!(
            out.contains("2 functions affected across 2 hops (method calls matched by name)"),
            "{out}"
        );

        // A second hop through plain functions has nothing to say.
        std::fs::write(
            dir.path().join("free.py"),
            "def leaf():\n    return 1\n\n\ndef mid():\n    return leaf()\n\n\ndef top():\n    return mid()\n",
        )
        .unwrap();
        let out = search_callers_expanded(
            "leaf",
            Walk::new(dir.path(), WalkConfig::default()),
            &bloom,
            0,
            None,
            false,
        )
        .unwrap();
        assert!(
            out.contains("2 functions affected across 2 hops.\n"),
            "{out}"
        );
    }
}
