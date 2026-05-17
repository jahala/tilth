#![no_main]
//! Fuzz `strip_noise` across each StripLang variant (via extension dispatch).
//!
//! Strip handles comment removal + debug-log line detection + consecutive
//! blank collapsing. Past bugs have lived in multi-byte UTF-8 handling —
//! a single panic on a `.split_at()` at a non-char boundary would surface
//! here.

use std::path::Path;
use libfuzzer_sys::fuzz_target;
use tilth::__fuzz::strip_noise;

const PATHS: &[&str] = &[
    "a.rs",
    "a.py",
    "a.go",
    "a.js",
    "a.ts",
    "a.txt", // falls through to None lang
];

fuzz_target!(|data: &[u8]| {
    let Ok(content) = std::str::from_utf8(data) else {
        return;
    };
    let line_count = content.lines().count() as u32;
    if line_count == 0 {
        return;
    }
    for p in PATHS {
        // Exercise a few representative def_range shapes.
        let _ = strip_noise(content, Path::new(p), Some((1, line_count)));
        let _ = strip_noise(content, Path::new(p), Some((1, line_count.min(5))));
        let _ = strip_noise(content, Path::new(p), None);
    }
});
