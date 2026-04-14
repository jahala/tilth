use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use crate::cache::OutlineCache;
use crate::lang::detect_file_type;
use crate::read::outline;
use crate::types::{estimate_tokens, FileType};

/// Generate a structural codebase map.
/// Code files show symbol names from outline cache.
/// Non-code files show name + token estimate.
#[must_use]
pub fn generate(scope: &Path, depth: usize, budget: Option<u64>, cache: &OutlineCache) -> String {
    let mut tree: BTreeMap<PathBuf, Vec<FileEntry>> = BTreeMap::new();

    let walker = WalkBuilder::new(scope)
        .follow_links(true)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .ignore(false)
        .parents(false)
        .filter_entry(|entry| {
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                if let Some(name) = entry.file_name().to_str() {
                    return !crate::search::SKIP_DIRS.contains(&name);
                }
            }
            true
        })
        .max_depth(Some(depth + 1))
        .build();

    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        let path = entry.path();
        let rel = path.strip_prefix(scope).unwrap_or(path);

        // Skip if deeper than requested
        let file_depth = rel.components().count().saturating_sub(1);
        if file_depth > depth {
            continue;
        }

        let parent = rel.parent().unwrap_or(Path::new("")).to_path_buf();
        let name = rel
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        let meta = std::fs::metadata(path).ok();
        let byte_len = meta.as_ref().map_or(0, std::fs::Metadata::len);
        let tokens = estimate_tokens(byte_len);

        let file_type = detect_file_type(path);
        let symbols = match file_type {
            FileType::Code(_) => {
                let mtime = meta
                    .and_then(|m| m.modified().ok())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

                let outline_str = cache.get_or_compute(path, mtime, || {
                    let content = std::fs::read_to_string(path).unwrap_or_default();
                    let buf = content.as_bytes();
                    outline::generate(path, file_type, &content, buf, true)
                });

                Some(extract_symbol_names(&outline_str))
            }
            _ => None,
        };

        tree.entry(parent.clone()).or_default().push(FileEntry {
            name,
            symbols,
            tokens,
        });

        // Ensure all ancestor directories exist in the tree so format_tree can find them.
        let mut ancestor = parent.parent();
        while let Some(a) = ancestor {
            tree.entry(a.to_path_buf()).or_default();
            if a == Path::new("") {
                break;
            }
            ancestor = a.parent();
        }
    }

    let mut out = format!("# Map: {} (depth {})\n", scope.display(), depth);
    format_tree(&tree, Path::new(""), 0, &mut out);

    match budget {
        Some(b) => crate::budget::apply(&out, b),
        None => out,
    }
}

struct FileEntry {
    name: String,
    symbols: Option<Vec<String>>,
    tokens: u64,
}

/// Extract symbol names from an outline string.
/// Outline lines look like: `[7-57]       fn classify`
/// We extract the last word(s) after the kind keyword.
fn extract_symbol_names(outline: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in outline.lines() {
        let trimmed = line.trim();
        // Skip import lines and empty lines
        if trimmed.starts_with('[') {
            // Find the symbol name after kind keywords
            if let Some(sig_start) = find_symbol_start(trimmed) {
                let sig = &trimmed[sig_start..];
                // Take just the name (up to first paren or space after name)
                let name = extract_name_from_sig(sig);
                if !name.is_empty() && name != "imports" {
                    names.push(name);
                }
            }
        }
    }
    names
}

fn find_symbol_start(line: &str) -> Option<usize> {
    let kinds = [
        "fn ",
        "struct ",
        "enum ",
        "trait ",
        "impl ",
        "mod ",
        "class ",
        "interface ",
        "type ",
        "const ",
        "static ",
        "function ",
        "method ",
        "def ",
    ];
    for kind in &kinds {
        if let Some(pos) = line.find(kind) {
            return Some(pos + kind.len());
        }
    }
    None
}

fn extract_name_from_sig(sig: &str) -> String {
    // Take characters until we hit a non-identifier char
    sig.chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect()
}

fn format_tree(
    tree: &BTreeMap<PathBuf, Vec<FileEntry>>,
    dir: &Path,
    indent: usize,
    out: &mut String,
) {
    // Collect subdirectories that have entries
    let mut subdirs: Vec<&PathBuf> = tree
        .keys()
        .filter(|k| k.parent() == Some(dir) && *k != dir)
        .collect();
    subdirs.sort();

    let prefix = "  ".repeat(indent);

    // Show files in this directory
    if let Some(files) = tree.get(dir) {
        for f in files {
            if let Some(ref symbols) = f.symbols {
                if symbols.is_empty() {
                    let _ = writeln!(out, "{prefix}{} (~{} tokens)", f.name, f.tokens);
                } else {
                    let syms = symbols.join(", ");
                    let truncated = if syms.len() > 80 {
                        format!("{}...", crate::types::truncate_str(&syms, 77))
                    } else {
                        syms
                    };
                    let _ = writeln!(out, "{prefix}{}: {truncated}", f.name);
                }
            } else {
                let _ = writeln!(out, "{prefix}{} (~{} tokens)", f.name, f.tokens);
            }
        }
    }

    // Recurse into subdirectories
    for subdir in subdirs {
        let dir_name = subdir.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        let _ = writeln!(out, "{prefix}{dir_name}/");
        format_tree(tree, subdir, indent + 1, out);
    }
}
