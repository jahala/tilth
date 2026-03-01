//! Resolve import statements to local file paths.
//! Used by the MCP layer to hint related files after an outlined read.

use std::fs;
use std::path::{Path, PathBuf};

use crate::read::detect_file_type;
use crate::types::{FileType, Lang};

const MAX_SUGGESTIONS: usize = 8;

/// Extract import sources from a code file and resolve them to existing local file paths.
/// Returns empty Vec for non-code files, files with no imports, or when all imports are external.
pub fn resolve_related_files(file_path: &Path) -> Vec<PathBuf> {
    let Ok(content) = fs::read_to_string(file_path) else {
        return Vec::new();
    };
    resolve_related_files_with_content(file_path, &content)
}

/// Same as `resolve_related_files` but takes pre-read content to avoid a redundant file read.
pub fn resolve_related_files_with_content(file_path: &Path, content: &str) -> Vec<PathBuf> {
    let FileType::Code(lang) = detect_file_type(file_path) else {
        return Vec::new();
    };

    let Some(dir) = file_path.parent() else {
        return Vec::new();
    };

    let mut results = Vec::new();
    for line in content.lines() {
        if results.len() >= MAX_SUGGESTIONS {
            break;
        }
        if !is_import_line(line, lang) {
            continue;
        }
        let source = super::outline::code::extract_import_source(line);
        if source.is_empty() || is_external(&source, lang) {
            continue;
        }
        if let Some(path) = resolve(dir, &source, lang) {
            if !results.contains(&path) {
                results.push(path);
            }
        }
    }
    results
}

fn is_import_line(line: &str, lang: Lang) -> bool {
    let trimmed = line.trim_start();
    match lang {
        Lang::Rust => trimmed.starts_with("use "),
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
            trimmed.starts_with("import ") || trimmed.starts_with("import{")
        }
        Lang::Python => trimmed.starts_with("import ") || trimmed.starts_with("from "),
        Lang::Go | Lang::Java | Lang::Scala | Lang::Kotlin => trimmed.starts_with("import "),
        Lang::C | Lang::Cpp => trimmed.starts_with("#include"),
        _ => false,
    }
}

fn is_external(source: &str, lang: Lang) -> bool {
    match lang {
        Lang::Rust => {
            !(source.starts_with("crate::")
                || source.starts_with("self::")
                || source.starts_with("super::"))
        }
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
            !(source.starts_with('.') || source.starts_with("@/") || source.starts_with("~/"))
        }
        Lang::Python => !source.starts_with('.'),
        Lang::C | Lang::Cpp => !source.starts_with('"'),
        // Go, Java, Scala, Kotlin — can't resolve without build system knowledge.
        _ => true,
    }
}

fn resolve(dir: &Path, source: &str, lang: Lang) -> Option<PathBuf> {
    match lang {
        Lang::Rust => resolve_rust(dir, source),
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => resolve_js(dir, source),
        Lang::Python => resolve_python(dir, source),
        Lang::C | Lang::Cpp => resolve_c_include(dir, source),
        _ => None,
    }
}

// --- Rust ---

fn resolve_rust(dir: &Path, source: &str) -> Option<PathBuf> {
    if let Some(rest) = source.strip_prefix("crate::") {
        let src_dir = find_src_ancestor(dir)?;
        try_rust_path(src_dir, rest)
    } else if let Some(rest) = source.strip_prefix("self::") {
        try_rust_path(dir, rest)
    } else if let Some(rest) = source.strip_prefix("super::") {
        try_rust_path(dir.parent()?, rest)
    } else {
        None
    }
}

/// Try progressively shorter paths until one resolves.
/// `cache::OutlineCache` → try cache/OutlineCache.rs (no) → cache.rs (yes).
/// `read::imports` → try read/imports.rs (yes) → stop.
fn try_rust_path(base: &Path, rest: &str) -> Option<PathBuf> {
    let segments: Vec<&str> = rest.split("::").collect();
    for len in (1..=segments.len()).rev() {
        let rel: PathBuf = segments[..len].iter().collect();
        if let Some(found) = try_rust_module(&base.join(&rel)) {
            return Some(found);
        }
    }
    None
}

fn try_rust_module(base: &Path) -> Option<PathBuf> {
    let with_rs = base.with_extension("rs");
    if with_rs.exists() {
        return Some(with_rs);
    }
    let mod_rs = base.join("mod.rs");
    if mod_rs.exists() {
        return Some(mod_rs);
    }
    None
}

fn find_src_ancestor(start: &Path) -> Option<&Path> {
    let mut current = start;
    loop {
        if current.file_name().and_then(|n| n.to_str()) == Some("src") {
            return Some(current);
        }
        current = current.parent()?;
    }
}

// --- JS/TS ---

fn resolve_js(dir: &Path, source: &str) -> Option<PathBuf> {
    let base = dir.join(source);
    // Try with extensions
    for ext in &[".ts", ".tsx", ".js", ".jsx"] {
        let candidate = PathBuf::from(format!("{}{ext}", base.display()));
        if candidate.exists() {
            return Some(candidate);
        }
    }
    // Already has extension
    if base.exists() && base.is_file() {
        return Some(base);
    }
    // Index files
    for name in &["index.ts", "index.tsx", "index.js", "index.jsx"] {
        let candidate = base.join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

// --- Python ---

fn resolve_python(dir: &Path, source: &str) -> Option<PathBuf> {
    let dots = source.bytes().take_while(|&b| b == b'.').count();
    if dots == 0 {
        return None;
    }
    // Each dot beyond the first goes up one directory.
    let mut base = dir.to_path_buf();
    for _ in 1..dots {
        base = base.parent()?.to_path_buf();
    }
    let module_part = &source[dots..];
    if module_part.is_empty() {
        // Bare `from . import X`
        let init = base.join("__init__.py");
        return if init.exists() { Some(init) } else { None };
    }
    let rel = module_part.replace('.', "/");
    let as_file = base.join(format!("{rel}.py"));
    if as_file.exists() {
        return Some(as_file);
    }
    let as_pkg = base.join(&rel).join("__init__.py");
    if as_pkg.exists() {
        return Some(as_pkg);
    }
    None
}

// --- C/C++ ---

fn resolve_c_include(dir: &Path, source: &str) -> Option<PathBuf> {
    let clean = source.trim_matches('"');
    let candidate = dir.join(clean);
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}
