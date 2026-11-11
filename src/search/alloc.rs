//! Value-based budget allocation for search output.
//!
//! When assembled search output exceeds the token budget, `budget::apply`'s
//! position-based tail-cut drops whatever sorts last — but faceted output can
//! place a high-value match in the final facet. This keeps blocks by information
//! value instead, so the most relevant matches survive regardless of where they
//! land in the rendered order.

use std::fmt::Write;

use crate::types::estimate_tokens;

use super::Expansion;

/// One match block in assembled search output: its information value and its
/// byte range `start..end`. `body_start` is where the block's expanded source
/// begins, or `end` when it has none. Over budget, a block can keep
/// `start..body_start` and lose its expansion.
#[derive(Debug)]
pub(super) struct Block {
    pub value: i64,
    pub start: usize,
    pub body_start: usize,
    pub end: usize,
    pub expansion: Option<Expansion>,
}

/// What survives of one block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    Whole,
    /// The block's lines before its expansion.
    WithoutBody,
    Dropped,
}

/// Choose what to keep of each block within `budget` tokens, preferring
/// higher value.
///
/// `items` is `(value, tokens, tokens_without_body)` per block in rendered
/// order; higher `value` means more worth keeping. Returns one `Fit` per item,
/// in the same order.
///
/// Strategy: first-fit by value. Blocks are considered in descending value
/// order. Each is kept whole if it fits in what is left of the budget, else
/// without its body if that fits, else dropped. A smaller, lower-value block
/// can still be kept after a larger higher-value one was cut, so leftover
/// budget is not wasted. Deterministic: ties broken by original index.
///
/// When the total cost is within budget every block is kept whole, so the
/// caller's render is byte-identical to the un-budgeted output.
pub fn select_within_budget(items: &[(i64, u64, u64)], budget: u64) -> Vec<Fit> {
    // Fast path: everything fits → keep all (byte-identical render). Saturating
    // so a future caller passing pathological token counts can't overflow-panic
    // in debug (within `fit_to_budget` the sum is bounded by the body length).
    let total: u64 = items
        .iter()
        .fold(0u64, |acc, &(_, tokens, _)| acc.saturating_add(tokens));
    if total <= budget {
        return vec![Fit::Whole; items.len()];
    }

    let mut fit = vec![Fit::Dropped; items.len()];
    // Consider blocks by descending value, stable on original index.
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&a, &b| items[b].0.cmp(&items[a].0).then(a.cmp(&b)));

    let mut remaining = budget;
    for i in order {
        let (_, tokens, without_body) = items[i];
        if tokens <= remaining {
            fit[i] = Fit::Whole;
            remaining -= tokens;
        } else if without_body < tokens && without_body <= remaining {
            fit[i] = Fit::WithoutBody;
            remaining -= without_body;
        }
        // else: dropped — a smaller lower-value block may still fit.
    }
    fit
}

/// The line that replaces an expansion a budget cut.
fn body_omitted_line(tokens: u64) -> String {
    format!("\n... expansion omitted to fit budget (~{tokens} tokens)")
}

/// Fit assembled search output to `budget_tokens`, keeping the highest-value
/// match blocks. `blocks` are in ascending `start` order with no overlap;
/// every other byte is structural (headers, facet labels, hidden-tails) and
/// always kept.
///
/// When `body` is already within budget it is returned UNCHANGED (byte-identical
/// to the streamed output). Over budget, the lowest-value blocks lose their
/// expansion first and are dropped after that (not the trailing ones), so the
/// most relevant matches survive whichever facet they landed in, and a match
/// whose expansion alone overruns the budget still shows where it is. Byte
/// ranges come from `String::len()` snapshots taken during assembly, so every
/// slice lands on a char boundary. Surviving expansion offsets are rebased
/// to the returned text; omitted expansions are cleared.
pub(super) fn fit_to_budget(body: &str, blocks: &mut [Block], budget_tokens: u64) -> String {
    // Invariant the slicing below relies on: blocks are ascending, non-overlapping,
    // within `body`, and on char boundaries. They come from `String::len()`
    // snapshots taken during assembly; assert it in debug so a future regression in
    // the segment recorder trips here rather than panicking on a bad slice in
    // release.
    #[cfg(debug_assertions)]
    {
        let mut prev_end = 0usize;
        for b in blocks.iter() {
            debug_assert!(b.start >= prev_end, "blocks overlap or are out of order");
            debug_assert!(
                b.start <= b.body_start && b.body_start <= b.end,
                "block body start outside the block"
            );
            debug_assert!(b.end <= body.len(), "block end exceeds body length");
            for at in [b.start, b.body_start, b.end] {
                debug_assert!(body.is_char_boundary(at), "block offset off char boundary");
            }
            prev_end = b.end;
        }
    }

    if estimate_tokens(body.len() as u64) <= budget_tokens {
        return body.to_string();
    }

    // Structural bytes (everything outside a block) are always kept; the value
    // selection only spends the budget left after reserving them.
    let block_bytes: usize = blocks.iter().map(|b| b.end.saturating_sub(b.start)).sum();
    let structural_tokens = estimate_tokens(body.len().saturating_sub(block_bytes) as u64);

    // If structural bytes alone already meet or exceed the budget, value selection
    // can't help: every block would be dropped, leaving a still-over-budget string
    // plus a misleading "lower-value omitted" note. Hand the whole body back and
    // let `budget::apply`'s position cut handle it.
    if structural_tokens >= budget_tokens {
        return body.to_string();
    }

    let block_budget = budget_tokens.saturating_sub(structural_tokens);

    let items: Vec<(i64, u64, u64)> = blocks
        .iter()
        .map(|b| {
            let whole = estimate_tokens(b.end.saturating_sub(b.start) as u64);
            let without_body = if b.body_start < b.end {
                let omitted = estimate_tokens((b.end - b.body_start) as u64);
                estimate_tokens((b.body_start - b.start + body_omitted_line(omitted).len()) as u64)
            } else {
                whole
            };
            (b.value, whole, without_body)
        })
        .collect();
    let fit = select_within_budget(&items, block_budget);

    let mut out = String::with_capacity(body.len());
    let mut cursor = 0usize;
    let mut dropped = 0usize;
    for (b, fit) in blocks.iter_mut().zip(&fit) {
        out.push_str(&body[cursor..b.start]); // structural gap before this block
        match fit {
            Fit::Whole => {
                if let Some(expansion) = &mut b.expansion {
                    expansion.end = out.len() + (expansion.end - b.start);
                }
                out.push_str(&body[b.start..b.end]);
            }
            Fit::WithoutBody => {
                b.expansion = None;
                out.push_str(&body[b.start..b.body_start]);
                let omitted = estimate_tokens((b.end - b.body_start) as u64);
                out.push_str(&body_omitted_line(omitted));
            }
            Fit::Dropped => {
                b.expansion = None;
                dropped += 1;
            }
        }
        cursor = b.end;
    }
    out.push_str(&body[cursor..]); // trailing structural bytes

    if dropped > 0 {
        let _ = write!(
            out,
            "\n\n... {dropped} lower-value match(es) omitted to fit budget"
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Blocks without an expansion: each is kept whole or dropped.
    fn keep(items: &[(i64, u64)], budget: u64) -> Vec<bool> {
        let items: Vec<(i64, u64, u64)> = items.iter().map(|&(v, t)| (v, t, t)).collect();
        select_within_budget(&items, budget)
            .into_iter()
            .map(|f| f == Fit::Whole)
            .collect()
    }

    /// A block that cannot fit whole keeps its lines before the expansion,
    /// and then a lower-value block can still use what is left.
    #[test]
    fn a_block_too_big_to_keep_whole_keeps_its_head() {
        let items = [(80, 600, 20), (0, 30, 30)];
        assert_eq!(
            select_within_budget(&items, 100),
            vec![Fit::WithoutBody, Fit::Whole]
        );
    }

    /// Simple constructor for a block with no expansion.
    fn block(value: i64, start: usize, end: usize) -> Block {
        Block {
            value,
            start,
            body_start: end,
            end,
            expansion: None,
        }
    }

    #[test]
    fn keeps_all_when_under_budget() {
        let items = [(10, 30), (5, 30), (1, 30)];
        assert_eq!(keep(&items, 1000), vec![true, true, true]);
    }

    #[test]
    fn drops_lower_value_when_over_budget() {
        // Each block costs 100; budget fits exactly one → the highest-value one.
        let items = [(1, 100), (10, 100), (5, 100)];
        assert_eq!(keep(&items, 100), vec![false, true, false]);
    }

    #[test]
    fn first_fit_uses_leftover_for_smaller_lower_value_block() {
        // Value order: (10,60) kept (rem 40); (5,50) skipped (50 > 40);
        // (1,30) kept (30 <= 40) — leftover budget is not wasted.
        let items = [(10, 60), (5, 50), (1, 30)];
        assert_eq!(keep(&items, 100), vec![true, false, true]);
    }

    #[test]
    fn ties_break_by_original_index() {
        // Two equal-value blocks, budget fits one → the earlier index wins.
        let items = [(7, 100), (7, 100)];
        assert_eq!(keep(&items, 100), vec![true, false]);
    }

    #[test]
    fn zero_budget_keeps_nothing_costly() {
        let items = [(10, 1), (5, 1)];
        assert_eq!(keep(&items, 0), vec![false, false]);
    }

    #[test]
    fn zero_cost_block_always_kept() {
        // A zero-token block fits any budget, even 0.
        let items = [(1, 0), (10, 50)];
        assert_eq!(keep(&items, 0), vec![true, false]);
    }

    #[test]
    fn empty_input_is_empty() {
        assert_eq!(keep(&[], 100), Vec::<bool>::new());
    }

    #[test]
    fn negative_values_are_ordered_correctly() {
        // Penalised (negative-value) blocks are dropped before positive ones.
        let items = [(-5, 100), (3, 100)];
        assert_eq!(keep(&items, 100), vec![false, true]);
    }

    #[test]
    fn fit_to_budget_under_budget_is_byte_identical() {
        let body = "HEADER\n## a\nbody-a\n## b\nbody-b".to_string();
        let mut blocks = [block(1, 7, 20), block(2, 20, body.len())];
        assert_eq!(fit_to_budget(&body, &mut blocks, 100_000), body);
    }

    #[test]
    fn fit_to_budget_drops_low_value_keeps_high_and_structural() {
        // HEADER (structural) + two ~200-byte blocks. Budget fits structural +
        // one block only → the high-value one survives, the low-value one drops.
        let high = "AAAA".repeat(50);
        let low = "BBBB".repeat(50);
        let body = format!("HEADER\n{high}\n{low}");
        let a_start = body.find(&high).unwrap();
        let a_end = a_start + high.len();
        let b_start = body.find(&low).unwrap();
        let b_end = b_start + low.len();
        let mut blocks = [block(10, a_start, a_end), block(1, b_start, b_end)];

        // body ~408 bytes (~102 tokens); structural ~8 bytes (~2 tokens); each
        // block ~50 tokens. Budget 55 → fits structural + one block.
        let out = fit_to_budget(&body, &mut blocks, 55);
        assert!(
            out.starts_with("HEADER"),
            "structural header must survive: {out}"
        );
        assert!(out.contains(&high), "high-value block must survive: {out}");
        assert!(
            !out.contains("BBBB"),
            "low-value block must be dropped: {out}"
        );
        assert!(
            out.contains("omitted to fit budget"),
            "omitted marker expected: {out}"
        );
    }

    #[test]
    fn fit_to_budget_passes_through_when_structural_exceeds_budget() {
        // A long structural header dwarfs a tiny block. Even dropping the block
        // can't get under the budget, so value selection is useless: return the
        // body unchanged (no misleading "omitted" note) and let the caller's
        // position cut handle it.
        let header = "H".repeat(400); // ~100 tokens of structural bytes
        let body = format!("{header}## x\nbody-x");
        let blk_start = body.find("body-x").unwrap();
        let mut blocks = [block(1, blk_start, body.len())];
        // Budget 10 tokens « structural ~100 tokens.
        let out = fit_to_budget(&body, &mut blocks, 10);
        assert_eq!(
            out, body,
            "structural-over-budget must pass the body through unchanged"
        );
        assert!(
            !out.contains("omitted"),
            "no misleading omitted note when nothing was value-dropped: {out}"
        );
    }
}
