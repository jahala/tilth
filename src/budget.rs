use crate::types::estimate_tokens;

/// Default cap applied to MCP tool responses when no explicit `budget` is given.
/// Sits just under the host's ~25K-token tool-response limit so a broad
/// `tilth_read full:true`, `regex`, or `diff` can't blow the host budget.
pub const DEFAULT_BUDGET: u64 = 24_000;

/// Metadata returned alongside a truncated string so callers can surface
/// "we cut at line N of M" without re-parsing the output.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TruncationInfo {
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
pub(crate) fn apply_with_info(output: &str, budget: u64) -> (String, Option<TruncationInfo>) {
    truncate(output, budget, budget)
}

/// Per-section token cap when `n` batch items share `budget`: `budget / n`
/// (min 1) so every item is represented instead of early items starving
/// later ones.
pub(crate) fn item_budget(budget: u64, n: usize) -> u64 {
    (budget / (n.max(1) as u64)).max(1)
}

/// Truncate one batch section to its per-item `cap` while citing the total
/// `budget` (not the per-item share) as the lever in any truncation marker —
/// raising `budget` is what gives the section more room.
pub(crate) fn apply_item(output: &str, cap: u64, budget: u64) -> String {
    truncate(output, cap, budget).0
}

/// Core truncation: clip `output` to `cap` tokens, preferring a section
/// boundary, and cite `display_budget` as the lever in the marker. For a
/// single call `cap == display_budget`; for a batch section `cap` is the
/// per-item share while `display_budget` stays the agent-facing total.
fn truncate(output: &str, cap: u64, display_budget: u64) -> (String, Option<TruncationInfo>) {
    let current = estimate_tokens(output.len() as u64);
    if current <= cap {
        return (output.to_string(), None);
    }

    let header_reserve = 50u64;
    let content_budget = cap.saturating_sub(header_reserve);
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

    // Prefer section boundaries (\n\n##); reject position 0 so code-rendered
    // bodies (whose only \n\n is the header/body separator) don't return an
    // empty body. Fall back to safe_max to stay on a char boundary.
    let cut_point = truncated
        .rfind("\n\n##")
        .filter(|&p| p > 0)
        .or_else(|| truncated.rfind("\n\n").filter(|&p| p > 0))
        .or_else(|| truncated.rfind('\n'))
        .unwrap_or(safe_max);

    let clean_body = &body[..cut_point];

    let omitted_bytes = output.len() - header_end - cut_point;
    let remaining_tokens = estimate_tokens(omitted_bytes as u64);
    let result = format!(
        "{header}{clean_body}\n\n... truncated — raise `budget` (currently {display_budget}) or request less per call to see the remaining ~{remaining_tokens} tokens"
    );

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
    use std::fmt::Write as _;

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
            writeln!(input, "line {i}").unwrap();
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
    fn apply_with_info_emoji_no_newline_does_not_panic() {
        // Single-line UTF-8 with no \n in the truncated region: `max_bytes` may land
        // mid-codepoint, so the fallback must clamp to a char boundary.
        let body: String = "🦀".repeat(500); // 2000 bytes, no newlines
        let input = format!("# header\n{body}");
        // Pick a budget so max_bytes lands somewhere mid-crab.
        let (_out, info) = apply_with_info(&input, 100);
        assert!(info.is_some(), "expected truncation on tight budget");
    }

    #[test]
    fn apply_with_info_mixed_width_no_newline_does_not_panic() {
        // Issue #39 bug 2: with no `\n` anywhere, `header_end = 0`, `body` is the
        // whole output, and all three `rfind`s return None — so `.unwrap_or(safe_max)`
        // is the only thing keeping `&body[..max_bytes]` off a mid-codepoint slice.
        //
        // `max_bytes = content_budget * 4` is always a multiple of 4, so a body of
        // only 4-byte codepoints (🦀, 🍌) stays perfectly aligned and can never
        // witness the panic. Interleaving 1-byte letters knocks the char boundaries
        // off the ×4 grid, so the unclamped cut lands inside a 4-byte glyph (here,
        // mid-🍌 at bytes 122..126).
        let unit = "go🦀nuts🍌yo"; // 16 bytes: letters + crab + banana, no newline
        let input = unit.repeat(100); // 1600 bytes
        assert!(!input.contains('\n'), "test input must have no newline");

        // Mirror apply_with_info's budget math (header_reserve = 50, ×4 inverse of
        // estimate_tokens) and assert the pre-fix cut is genuinely mid-codepoint, so
        // this test can never silently go vacuous the way an all-🦀 body would.
        let budget = 81u64;
        let max_bytes = (budget as usize - 50) * 4; // = 124
        assert!(
            !input.is_char_boundary(max_bytes),
            "vacuous: max_bytes={max_bytes} sits on a char boundary — pick a mix that misaligns"
        );

        let (out, info) = apply_with_info(&input, budget);
        let info = info.expect("must truncate on tight budget");
        assert!(info.at_line >= 1, "line accounting stays sane");
        assert!(out.contains("truncated"), "marker line missing: {out}");
    }

    #[test]
    fn apply_with_info_code_format_survives_header_separator() {
        // Code-rendered output has `<n>:<hash>|<content>\n` on every line, so
        // the only `\n\n` in the body is the header/body separator at position
        // 0. Pre-fix, `rfind("\n\n")` returned 0 and `clean_body` was empty —
        // the response was just `<header>\n\n... truncated` regardless of how
        // generous the budget was. Verify the cut now lands deep enough that
        // real content survives.
        let mut input = String::from("# src/foo.rs (200 lines, ~2k tokens) [full]\n\n");
        for i in 1..=200 {
            writeln!(input, "{i}:abc|let x_{i} = {i};").unwrap();
        }
        // Tight budget — must truncate, but should leave room for many lines.
        let (out, info) = apply_with_info(&input, 400);
        let info = info.expect("must truncate at this budget");
        assert!(
            info.at_line > 5,
            "must cut deep into body, not at header separator: at_line={}",
            info.at_line
        );
        assert!(
            out.contains("1:abc|let x_1 ="),
            "first code line must survive truncation: {out}"
        );
    }

    #[test]
    fn apply_unchanged_for_backwards_compat() {
        // apply() must still return identical output to apply_with_info().0
        let mut input = String::from("# header\n");
        for i in 1..=50 {
            writeln!(input, "line {i}").unwrap();
        }
        let from_wrapper = apply(&input, 60);
        let (from_info, _) = apply_with_info(&input, 60);
        assert_eq!(from_wrapper, from_info);
    }

    #[test]
    fn item_budget_splits_evenly_with_floor() {
        assert_eq!(item_budget(300, 3), 100);
        assert_eq!(item_budget(10, 4), 2); // 10 / 4 = 2
        assert_eq!(item_budget(3, 10), 1); // floor of 1 — never starves to 0
        assert_eq!(item_budget(1000, 0), 1000); // n.max(1) guards div-by-zero
    }

    #[test]
    fn apply_item_cites_total_budget_not_per_item_share() {
        // A section larger than its per-item cap truncates, and the marker
        // must name the TOTAL budget (the real lever), not the per-item cap.
        let big = format!("# header\n{}", "x = 1;\n".repeat(2000));
        let out = apply_item(&big, 50, 24_000);
        let tail = &out[out.len().saturating_sub(160)..];
        assert!(out.contains("truncated"), "must truncate: {tail}");
        assert!(
            out.contains("currently 24000"),
            "must cite total budget, not the per-item cap (50): {tail}"
        );
        assert!(
            !out.contains("currently 50"),
            "must not leak the per-item cap as the lever: {tail}"
        );
    }
}
