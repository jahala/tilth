/// Any null byte in the first 512 bytes → binary.
/// Uses memchr for the scan — single SIMD pass, no branching.
pub fn is_binary(buf: &[u8]) -> bool {
    let window = &buf[..buf.len().min(512)];
    memchr::memchr(0, window).is_some()
}

/// Check filename against known generated/lock files.
pub fn is_generated_by_name(name: &str) -> bool {
    matches!(
        name,
        "package-lock.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "Cargo.lock"
            | "composer.lock"
            | "Gemfile.lock"
            | "poetry.lock"
            | "go.sum"
            | "bun.lockb"
    )
}

const GENERATED_MARKERS: &[&[u8]] = &[
    b"@generated",
    b"DO NOT EDIT",
    b"Do not edit",
    b"do not edit",
    b"auto-generated",
    b"Auto-generated",
    b"AUTO-GENERATED",
    b"this file is generated",
    b"This file is generated",
    b"THIS FILE IS GENERATED",
    b"automatically generated",
    b"Automatically generated",
];

/// Scan first 512 bytes for generated-file markers using SIMD memmem.
pub fn is_generated_by_content(buf: &[u8]) -> bool {
    let window = &buf[..buf.len().min(512)];
    GENERATED_MARKERS
        .iter()
        .any(|m| memchr::memmem::find(window, m).is_some())
}
