//! Smart truncation — for functions >80 lines, select maximally diverse/important
//! lines instead of showing everything. Caps at ~40 lines to reduce token cost
//! while preserving the most useful signal.
//!
//! All detection is line-by-line text matching; no tree-sitter needed.

use crate::types::Lang;

/// Minimum function size (in lines) before smart truncation kicks in.
const SMART_TRUNCATE_MIN_LINES: u32 = 80;

/// Maximum number of lines to keep after truncation.
const SMART_TRUNCATE_MAX_LINES: usize = 40;

/// Select diverse/important lines from a function body.
///
/// Returns `None` if the range is smaller than [`SMART_TRUNCATE_MIN_LINES`]
/// (no truncation needed). Otherwise returns `Some(vec)` of 1-based line
/// numbers to KEEP, sorted ascending.
pub(crate) fn select_diverse_lines(
    content: &str,
    start: u32,
    end: u32,
    _lang: Lang,
) -> Option<Vec<u32>> {
    if end.saturating_sub(start) < SMART_TRUNCATE_MIN_LINES {
        return None;
    }

    let lines: Vec<&str> = content.lines().collect();
    let mut scored: Vec<(u32, u32)> = Vec::new(); // (line_number, score)

    for line_num in start..=end {
        let idx = (line_num - 1) as usize;
        let line = match lines.get(idx) {
            Some(l) => *l,
            None => break,
        };
        let trimmed = line.trim();
        let score = score_line(trimmed, line_num, start, end);
        scored.push((line_num, score));
    }

    // Sort by (score DESC, line ASC) to pick highest-value lines first
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    // Take top N
    scored.truncate(SMART_TRUNCATE_MAX_LINES);

    // Re-sort by line number for reading order
    scored.sort_by_key(|&(line, _)| line);

    Some(scored.into_iter().map(|(line, _)| line).collect())
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

    // Function calls: contains `(` but not just simple assignment without call
    if trimmed.contains('(') && !is_plain_assignment(trimmed) {
        score = score.max(10);
    }

    // Assignment with function call on RHS (score 5)
    if trimmed.contains('=') && trimmed.contains('(') {
        score = score.max(5);
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

/// Returns `true` if the line is a plain assignment with no function call.
fn is_plain_assignment(trimmed: &str) -> bool {
    trimmed.contains('=') && !trimmed.contains('(')
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

    #[test]
    fn short_function_returns_none() {
        let content = (1..=50)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = select_diverse_lines(&content, 1, 50, Lang::Rust);
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

        let result = select_diverse_lines(&content, 1, 100, Lang::Rust);
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
        let result = select_diverse_lines(&content, 1, 91, Lang::Rust).unwrap();

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
        let result = select_diverse_lines(&content, 1, 91, Lang::Rust).unwrap();

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

        let result = select_diverse_lines(&content, 1, 100, Lang::Rust).unwrap();

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
        let result = select_diverse_lines(&content, 1, 81, Lang::Rust);
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
        let result = select_diverse_lines(&content, 1, 80, Lang::Rust);
        assert!(
            result.is_none(),
            "79-line gap should not trigger truncation"
        );
    }
}
