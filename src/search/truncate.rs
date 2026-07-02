//! Smart truncation — for functions >80 lines, select maximally diverse/important
//! lines instead of showing everything. Caps at ~40 lines to reduce token cost
//! while preserving the most useful signal.
//!
//! All detection is line-by-line text matching; no tree-sitter needed.

/// Score boost applied to lines that contain the searched symbol.
const QUERY_MATCH_BOOST: u32 = 50;

/// Score boost applied to lines immediately adjacent (±1) to a matching line.
const QUERY_PROXIMITY_BOOST: u32 = 5;

/// Minimum function size (in lines) before smart truncation kicks in.
const SMART_TRUNCATE_MIN_LINES: u32 = 80;

/// Maximum number of lines to keep after truncation.
const SMART_TRUNCATE_MAX_LINES: usize = 40;

/// Select diverse/important lines from a function body.
///
/// Returns `None` if the range is smaller than [`SMART_TRUNCATE_MIN_LINES`]
/// (no truncation needed). Otherwise returns `Some(vec)` of 1-based line
/// numbers to KEEP, sorted ascending.
///
/// When `query` is `Some(symbol)`, lines containing the symbol (case-sensitive
/// substring) receive a [`QUERY_MATCH_BOOST`] on top of their structural score,
/// and their immediate neighbours (±1) receive a [`QUERY_PROXIMITY_BOOST`].
/// When `query` is `None`, behaviour is byte-identical to the previous version.
pub(crate) fn select_diverse_lines(
    content: &str,
    start: u32,
    end: u32,
    query: Option<&str>,
) -> Option<Vec<u32>> {
    if end.saturating_sub(start) < SMART_TRUNCATE_MIN_LINES {
        return None;
    }

    // Bisect kill-switch: force the query-agnostic path (byte-identical to the
    // pre-feature selector, per the doc above).
    let query = if crate::types::feature_disabled("TILTH_NO_QTRUNC") {
        None
    } else {
        query
    };

    let lines: Vec<&str> = content.lines().collect();
    // Treat an empty query as no query, so the boost pass below is skipped and
    // behaviour stays byte-identical to the `None` path.
    let query = query.filter(|q| !q.is_empty());
    let mut scored: Vec<(u32, u32, bool)> = Vec::new(); // (line_number, score, matches_query)

    // Pass 1: structural scoring + query-match detection in a single walk. Each
    // line is read once here; the match flag is recorded so Pass 2 never has to
    // re-read `lines`.
    for line_num in start..=end {
        let idx = (line_num - 1) as usize;
        let line = match lines.get(idx) {
            Some(l) => *l,
            None => break,
        };
        let trimmed = line.trim();
        let score = score_line(trimmed, line_num, start, end);
        let matches_query = query.is_some_and(|q| line.contains(q));
        scored.push((line_num, score, matches_query));
    }

    // Pass 2: query-aware boost — lift lines that contain the searched symbol
    // and their immediate neighbours (±1). Skipped entirely when there is no
    // query, keeping the `None` path byte-identical.
    if query.is_some() {
        for i in 0..scored.len() {
            if scored[i].2 {
                scored[i].1 = scored[i].1.saturating_add(QUERY_MATCH_BOOST);
                if i > 0 {
                    scored[i - 1].1 = scored[i - 1].1.saturating_add(QUERY_PROXIMITY_BOOST);
                }
                if i + 1 < scored.len() {
                    scored[i + 1].1 = scored[i + 1].1.saturating_add(QUERY_PROXIMITY_BOOST);
                }
            }
        }
    }

    // Sort by (score DESC, line ASC) to pick highest-value lines first
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    // Take top N
    scored.truncate(SMART_TRUNCATE_MAX_LINES);

    // Re-sort by line number for reading order
    scored.sort_by_key(|&(line, _, _)| line);

    Some(scored.into_iter().map(|(line, _, _)| line).collect())
}

/// Score a single line based on its content. Higher scores indicate more
/// important lines that should be preserved during truncation.
fn score_line(trimmed: &str, line_num: u32, start: u32, end: u32) -> u32 {
    // Signature and closing brace are always kept
    if line_num == start || line_num == end {
        return 100;
    }

    // Blank lines and comment-only lines get zero
    if trimmed.is_empty() {
        return 0;
    }
    if is_comment_only(trimmed) {
        return 0;
    }

    let mut score: u32 = 0;

    // Control flow keywords (score 10)
    if is_control_flow(trimmed) {
        score = score.max(10);
    }

    // Error handling (score 10)
    if is_error_handling(trimmed) {
        score = score.max(10);
    }

    // Function calls: contains `(`
    if trimmed.contains('(') {
        score = score.max(10);
    }

    // Struct/map construction: ends with `{` but isn't just an opening brace
    if trimmed.ends_with('{') && trimmed.len() > 1 {
        score = score.max(5);
    }

    // Simple assignments / variable declarations (score 1)
    if score == 0 && (trimmed.contains('=') || is_var_decl(trimmed)) {
        score = 1;
    }

    score
}

/// Returns `true` if the line is comment-only (any common language).
fn is_comment_only(trimmed: &str) -> bool {
    trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with("* ")
        || trimmed == "*/"
        || trimmed == "*"
}

/// Returns `true` if the line starts with a control flow keyword.
fn is_control_flow(trimmed: &str) -> bool {
    trimmed.starts_with("if ")
        || trimmed.starts_with("} else")
        || trimmed.starts_with("else ")
        || trimmed.starts_with("else{")
        || trimmed == "else"
        || trimmed.starts_with("match ")
        || trimmed.starts_with("switch ")
        || trimmed.starts_with("case ")
        || trimmed.starts_with("for ")
        || trimmed.starts_with("while ")
        || trimmed.starts_with("loop ")
        || trimmed.starts_with("loop{")
        || trimmed == "loop"
        || trimmed.starts_with("return ")
        || trimmed == "return"
        || trimmed.starts_with("return;")
}

/// Returns `true` if the line contains error handling patterns.
fn is_error_handling(trimmed: &str) -> bool {
    trimmed.ends_with("?;")
        || trimmed.ends_with('?')
        || trimmed.contains(".unwrap()")
        || trimmed.contains(".expect(")
        || trimmed.starts_with("catch ")
        || trimmed.starts_with("catch(")
        || trimmed.starts_with("except ")
        || trimmed.starts_with("except:")
        || trimmed.contains("panic!(")
        || trimmed.contains("bail!(")
        || trimmed.contains("anyhow!(")
}

/// Returns `true` if the line starts with a variable declaration keyword.
fn is_var_decl(trimmed: &str) -> bool {
    trimmed.starts_with("let ")
        || trimmed.starts_with("const ")
        || trimmed.starts_with("var ")
        || trimmed.starts_with("mut ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Existing tests — all pass `None` for the new `query` parameter ──────

    #[test]
    fn short_function_returns_none() {
        let content = (1..=50)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = select_diverse_lines(&content, 1, 50, None);
        assert!(
            result.is_none(),
            "functions <80 lines should not be truncated"
        );
    }

    #[test]
    fn long_function_returns_some() {
        let mut lines: Vec<String> = Vec::new();
        lines.push("fn big_function() {".to_owned());
        for i in 2..=99 {
            lines.push(format!("    let x{i} = {i};"));
        }
        lines.push("}".to_owned());
        let content = lines.join("\n");

        let result = select_diverse_lines(&content, 1, 100, None);
        assert!(result.is_some(), "functions >=80 lines should be truncated");

        let kept = result.unwrap();
        assert!(kept.len() <= SMART_TRUNCATE_MAX_LINES);
        // Signature and closing line must be included
        assert!(kept.contains(&1), "signature line must be kept");
        assert!(kept.contains(&100), "closing line must be kept");
        // Must be sorted ascending
        assert!(kept.windows(2).all(|w| w[0] < w[1]), "lines must be sorted");
    }

    #[test]
    fn control_flow_lines_preferred() {
        let mut lines: Vec<String> = Vec::new();
        lines.push("fn example() {".to_owned());
        // Fill with low-value lines
        for i in 2..=90 {
            lines.push(format!("    // comment {i}"));
        }
        // Insert high-value control flow at specific positions
        lines[10] = "    if x > 0 {".to_owned(); // line 11
        lines[20] = "    match value {".to_owned(); // line 21
        lines[30] = "    return result;".to_owned(); // line 31
        lines[40] = "    for item in list {".to_owned(); // line 41
        lines.push("}".to_owned()); // line 91

        let content = lines.join("\n");
        let result = select_diverse_lines(&content, 1, 91, None).unwrap();

        assert!(result.contains(&11), "if-line should be kept");
        assert!(result.contains(&21), "match-line should be kept");
        assert!(result.contains(&31), "return-line should be kept");
        assert!(result.contains(&41), "for-line should be kept");
    }

    #[test]
    fn error_handling_lines_preferred() {
        let mut lines: Vec<String> = Vec::new();
        lines.push("fn example() {".to_owned());
        for _ in 2..=90 {
            lines.push(String::new()); // blank lines (score 0)
        }
        lines[15] = "    let x = foo()?;".to_owned(); // line 16
        lines[25] = "    bar.unwrap();".to_owned(); // line 26
        lines[35] = "    bail!(\"error\");".to_owned(); // line 36
        lines.push("}".to_owned());

        let content = lines.join("\n");
        let result = select_diverse_lines(&content, 1, 91, None).unwrap();

        assert!(result.contains(&16), "?; line should be kept");
        assert!(result.contains(&26), ".unwrap() line should be kept");
        assert!(result.contains(&36), "bail! line should be kept");
    }

    #[test]
    fn blank_and_comment_lines_deprioritized() {
        let mut lines: Vec<String> = Vec::new();
        lines.push("fn example() {".to_owned());
        // Mix of blanks, comments, and actual code
        for i in 2..=99 {
            if i % 3 == 0 {
                lines.push(String::new()); // blank
            } else if i % 3 == 1 {
                lines.push(format!("    // comment {i}"));
            } else {
                lines.push(format!("    do_something_{i}();"));
            }
        }
        lines.push("}".to_owned());
        let content = lines.join("\n");

        let result = select_diverse_lines(&content, 1, 100, None).unwrap();

        // Function call lines (score 10) should dominate over blanks/comments (score 0)
        let has_fn_calls = result.iter().any(|&ln| {
            let idx = (ln - 1) as usize;
            content
                .lines()
                .nth(idx)
                .is_some_and(|l| l.contains("do_something"))
        });
        assert!(has_fn_calls, "function call lines should be preferred");
    }

    #[test]
    fn exactly_80_line_gap_triggers_truncation() {
        let lines: Vec<String> = (1..=81).map(|i| format!("line {i}")).collect();
        let content = lines.join("\n");
        // end - start = 81 - 1 = 80 => equals threshold => triggers
        let result = select_diverse_lines(&content, 1, 81, None);
        assert!(
            result.is_some(),
            "exactly 80-line gap should trigger truncation"
        );
    }

    #[test]
    fn boundary_79_line_gap_does_not_trigger() {
        let lines: Vec<String> = (1..=80).map(|i| format!("line {i}")).collect();
        let content = lines.join("\n");
        // end - start = 80 - 1 = 79 => below threshold
        let result = select_diverse_lines(&content, 1, 80, None);
        assert!(
            result.is_none(),
            "79-line gap should not trigger truncation"
        );
    }

    // ── QTRUNC tests — query-aware boost ────────────────────────────────────

    /// Core differential test: a plain assignment line carrying a unique token
    /// (`needle_marker`) would normally be dropped (score 1, outcompeted by
    /// higher-score lines). With `query = Some("needle_marker")` it must survive;
    /// with `query = None` it must not.
    #[test]
    fn query_boost_rescues_needle_line() {
        // Build a body of 100 lines (triggers truncation).
        // Lines 2..=99 are plain assignments; line 100 is closing `}`.
        // We plant `needle_marker` on line 50 (a low-score assignment line).
        // The other 97 interior lines are plain assignments too (also score 1),
        // so with 40 slots and 2 locked (start/end), only 38 interior lines
        // can be kept — the needle is not guaranteed to be among them without
        // the boost.
        let mut lines: Vec<String> = Vec::new();
        lines.push("fn example() {".to_owned()); // line 1
        for i in 2usize..=99 {
            if i == 50 {
                lines.push("    let needle_marker = 50;".to_owned()); // line 50
            } else {
                lines.push(format!("    let x{i} = {i};"));
            }
        }
        lines.push("}".to_owned()); // line 100
        let content = lines.join("\n");

        // Both selections are deterministic (stable sort, tie-broken by ascending
        // line number). With the boost, line 50 jumps above the score-1 crowd and
        // is kept; without it, the 38 interior slots fill with the lowest line
        // numbers (2..=39), so line 50 is dropped. Check the boosted case first.
        let with_query = select_diverse_lines(&content, 1, 100, Some("needle_marker")).unwrap();
        assert!(
            with_query.contains(&50),
            "query=Some: needle_marker line must be kept"
        );

        // Without query: all interior lines have identical structural score (1),
        // so the sort is stable by line number and the first 38 (lines 2..=39)
        // are kept. Line 50 falls outside that window.
        let without_query = select_diverse_lines(&content, 1, 100, None).unwrap();
        assert!(
            !without_query.contains(&50),
            "query=None: needle_marker line must NOT be kept (score too low)"
        );
    }

    /// Proximity test: the line immediately after the needle also survives with
    /// the query boost (QUERY_PROXIMITY_BOOST lifts it above competitors).
    #[test]
    fn query_boost_preserves_neighbour() {
        // Same setup as above but we also check line 51 (needle + 1).
        // With query=None, line 51 is just another score-1 line that falls
        // outside the first-38-lines window alongside line 50.
        let mut lines: Vec<String> = Vec::new();
        lines.push("fn example() {".to_owned()); // line 1
        for i in 2usize..=99 {
            if i == 50 {
                lines.push("    let needle_marker = 50;".to_owned()); // line 50
            } else {
                lines.push(format!("    let x{i} = {i};"));
            }
        }
        lines.push("}".to_owned()); // line 100
        let content = lines.join("\n");

        let with_query = select_diverse_lines(&content, 1, 100, Some("needle_marker")).unwrap();
        // Line 51 gets QUERY_PROXIMITY_BOOST (5) on top of its structural score (1)
        // => total 6, vs bare score 1 for lines 40+. It must survive.
        assert!(
            with_query.contains(&51),
            "query=Some: neighbour of needle (line 51) must be kept"
        );

        let without_query = select_diverse_lines(&content, 1, 100, None).unwrap();
        assert!(
            !without_query.contains(&51),
            "query=None: line 51 must NOT be kept without boost"
        );
    }

    /// Sanity: passing `query = Some("")` (empty string) behaves identically to `None`.
    #[test]
    fn empty_query_behaves_like_none() {
        let mut lines: Vec<String> = Vec::new();
        lines.push("fn example() {".to_owned());
        for i in 2usize..=99 {
            if i == 50 {
                lines.push("    let needle_marker = 50;".to_owned());
            } else {
                lines.push(format!("    let x{i} = {i};"));
            }
        }
        lines.push("}".to_owned());
        let content = lines.join("\n");

        let empty_q = select_diverse_lines(&content, 1, 100, Some("")).unwrap();
        let none_q = select_diverse_lines(&content, 1, 100, None).unwrap();
        assert_eq!(
            empty_q, none_q,
            "empty query must produce identical output to None"
        );
    }
}
