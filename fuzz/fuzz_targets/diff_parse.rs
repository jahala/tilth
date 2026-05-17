#![no_main]
//! Fuzz `parse_unified_diff` — tilth's own unified-diff parser.
//!
//! Unlike outline/strip, this parser is fully our code (not a tree-sitter
//! crate). Any panic, integer overflow, or hunk-attribution boundary bug
//! is on us. Inputs come from `git diff` output or `tilth diff` callers.

use libfuzzer_sys::fuzz_target;
use tilth::__fuzz::parse_unified_diff;

fuzz_target!(|data: &[u8]| {
    let Ok(raw) = std::str::from_utf8(data) else {
        return;
    };
    parse_unified_diff(raw);
});
