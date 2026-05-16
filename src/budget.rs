use crate::types::estimate_tokens;

/// Metadata returned alongside a truncated string so callers can surface
/// "we cut at line N of M" without re-parsing the output.
#[derive(Debug, Clone, Copy)]
pub struct TruncationInfo {
    /// 1-indexed line of the original output where the cut landed.
    pub at_line: u32,
    /// Total line count of the original output before truncation. Lets
    /// callers render "showing 1–N of M lines" without re-reading.
    pub original_line_count: u32,
}

/// Apply token budget to output. Works backwards from the cap:
/// 1. Reserve 50 tokens for header
/// 2. Truncate content at section boundaries to avoid broken output
/// 3. Never exceed the budget
pub fn apply(output: &str, budget: u64) -> String {
    apply_with_info(output, budget).0
}

/// Like [`apply`], but also returns truncation metadata when the budget
/// actually clips the input. Returns `(text, None)` when the budget is
/// roomy enough that nothing was cut.
pub fn apply_with_info(output: &str, budget: u64) -> (String, Option<TruncationInfo>) {
    let current = estimate_tokens(output.len() as u64);
    if current <= budget {
        return (output.to_string(), None);
    }

    let header_reserve = 50u64;
    let content_budget = budget.saturating_sub(header_reserve);
    let max_bytes = (content_budget * 4) as usize; // inverse of estimate_tokens

    // Find the first newline after the header (first line)
    let header_end = output.find('\n').unwrap_or(0);
    let header = &output[..header_end];
    let body = &output[header_end..];

    if body.len() <= max_bytes {
        return (output.to_string(), None);
    }

    let safe_max = body.floor_char_boundary(max_bytes);
    let truncated = &body[..safe_max];

    // Prefer section boundaries (\n\n##) to avoid cutting mid-match in search results
    let cut_point = truncated
        .rfind("\n\n##")
        .or_else(|| truncated.rfind("\n\n"))
        .or_else(|| truncated.rfind('\n'))
        .unwrap_or(max_bytes);

    let clean_body = &body[..cut_point];

    let omitted_bytes = output.len() - header_end - cut_point;
    let remaining_tokens = estimate_tokens(omitted_bytes as u64);
    let result = format!(
        "{header}{clean_body}\n\n... truncated ({remaining_tokens} tokens omitted, budget: {budget})"
    );

    // Count newlines in the kept portion so callers can show "truncated at
    // line N" without scanning the result themselves, and on the full input
    // so they can show "of M".
    let kept = &output[..header_end + cut_point];
    let at_line =
        u32::try_from(kept.bytes().filter(|&b| b == b'\n').count() + 1).unwrap_or(u32::MAX);
    let original_line_count =
        u32::try_from(output.bytes().filter(|&b| b == b'\n').count() + 1).unwrap_or(u32::MAX);

    (
        result,
        Some(TruncationInfo {
            at_line,
            original_line_count,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_with_info_roomy_budget_returns_none() {
        // 20 chars ≈ 5 tokens; budget of 1000 is way over.
        let (out, info) = apply_with_info("# header\nshort body\n", 1000);
        assert_eq!(out, "# header\nshort body\n");
        assert!(info.is_none(), "no truncation info when fits");
    }

    #[test]
    fn apply_with_info_tight_budget_emits_info_with_at_line_and_total() {
        // Build a multi-line body large enough to force truncation.
        let mut input = String::from("# header\n");
        for i in 1..=200 {
            input.push_str(&format!("line {i}\n"));
        }
        // Budget that forces a cut somewhere in the middle.
        let (out, info) = apply_with_info(&input, 80);
        let info = info.expect("must report truncation");
        assert!(info.original_line_count >= 200, "M >= original lines");
        assert!(
            info.at_line >= 2 && info.at_line < info.original_line_count,
            "N inside (1, M): at_line={}, M={}",
            info.at_line,
            info.original_line_count
        );
        assert!(out.contains("truncated"), "marker line missing: {out}");
    }

    #[test]
    fn apply_unchanged_for_backwards_compat() {
        // apply() must still return identical output to apply_with_info().0
        let mut input = String::from("# header\n");
        for i in 1..=50 {
            input.push_str(&format!("line {i}\n"));
        }
        let from_wrapper = apply(&input, 60);
        let (from_info, _) = apply_with_info(&input, 60);
        assert_eq!(from_wrapper, from_info);
    }
}
