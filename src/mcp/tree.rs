//! `tilth_list` tree output: directory tree with token-cost rollups.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::types::estimate_tokens;

#[derive(Debug, Default)]
struct DirNode {
    children_files: Vec<(String, u64)>, // (name, bytes)
    children_dirs: std::collections::BTreeMap<String, Box<DirNode>>,
    file_count: u64,
    total_bytes: u64,
}

impl DirNode {
    fn insert(&mut self, parts: &[&str], bytes: u64) {
        self.file_count += 1;
        self.total_bytes += bytes;
        match parts.len() {
            0 => {}
            1 => self.children_files.push((parts[0].to_string(), bytes)),
            _ => {
                let head = parts[0].to_string();
                let child = self.children_dirs.entry(head).or_default();
                child.insert(&parts[1..], bytes);
            }
        }
    }
}

/// Compact human token count, matching `map.rs::fmt_tokens`'s k/M scale and rounding.
fn fmt_tokens(t: u64) -> String {
    #[allow(clippy::cast_precision_loss)] // display-only; mantissa loss is fine for summaries
    let f = t as f64;
    if f >= 999_950.0 {
        format!("~{:.1}M tokens", f / 1_000_000.0)
    } else if t >= 1000 {
        format!("~{:.1}k tokens", f / 1_000.0)
    } else {
        format!("~{t} tokens")
    }
}

fn render_dir(name: &str, node: &DirNode, prefix: &str, out: &mut String, is_root: bool) {
    let total_tokens = estimate_tokens(node.total_bytes);
    if is_root {
        let _ = writeln!(
            out,
            "{name}/      {tok}   {n} files",
            tok = fmt_tokens(total_tokens),
            n = node.file_count
        );
    }

    let mut entries: Vec<(bool, String, u64, Option<&DirNode>)> = Vec::new();
    for (n, b) in &node.children_files {
        entries.push((false, n.clone(), *b, None));
    }
    for (n, child) in &node.children_dirs {
        entries.push((true, n.clone(), child.total_bytes, Some(child.as_ref())));
    }
    entries.sort_by(|a, b| a.1.cmp(&b.1));

    let n = entries.len();
    for (i, (is_dir, name, bytes, child)) in entries.iter().enumerate() {
        let last = i == n - 1;
        let connector = if last { "└── " } else { "├── " };
        let child_prefix = if last { "    " } else { "│   " };
        if *is_dir {
            let child = child.expect("dir entry has node");
            let _ = writeln!(
                out,
                "{prefix}{connector}{name}/      {tok}   {fc} files",
                tok = fmt_tokens(estimate_tokens(*bytes)),
                fc = child.file_count
            );
            let new_prefix = format!("{prefix}{child_prefix}");
            render_dir(name, child, &new_prefix, out, false);
        } else {
            let _ = writeln!(
                out,
                "{prefix}{connector}{name}      {tok}",
                tok = fmt_tokens(estimate_tokens(*bytes))
            );
        }
    }
}

/// Build a tree string from `(path, bytes)` pairs rooted at `scope`.
pub fn render_tree(scope: &Path, files: &[(PathBuf, u64)]) -> String {
    let mut root = DirNode::default();
    for (path, bytes) in files {
        let rel = path.strip_prefix(scope).unwrap_or(path);
        let parts: Vec<&str> = rel.iter().filter_map(|c| c.to_str()).collect();
        if parts.is_empty() {
            continue;
        }
        root.insert(&parts, *bytes);
    }
    let mut out = String::new();
    let root_name = scope.file_name().and_then(|n| n.to_str()).unwrap_or(".");
    render_dir(root_name, &root, "", &mut out, true);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_tree_groups_dirs_and_files() {
        let scope = PathBuf::from("/tmp/proj");
        let files = vec![
            (scope.join("src/a.rs"), 100),
            (scope.join("src/b.rs"), 200),
            (scope.join("README.md"), 50),
        ];
        let out = render_tree(&scope, &files);
        assert!(out.contains("src/"));
        assert!(out.contains("a.rs"));
        assert!(out.contains("README.md"));
    }

    #[test]
    fn render_tree_dir_rollup_equals_sum_of_child_tokens() {
        let scope = PathBuf::from("/tmp/proj2");
        let files = vec![
            (scope.join("src/a.rs"), 4000),
            (scope.join("src/b.rs"), 8000),
        ];
        let out = render_tree(&scope, &files);
        let expected = fmt_tokens(estimate_tokens(4000) + estimate_tokens(8000));
        assert!(
            out.contains(&format!("src/      {expected}")),
            "dir rollup should equal sum of child tokens: {out}"
        );
    }
}
