//! Project fingerprint for MCP initialization.
//! Gives agents instant orientation without a tool call.

use std::collections::HashMap;
use std::fmt::Write;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::lang::detect_file_type;
use crate::read::imports::is_import_line;
use crate::search::SKIP_DIRS;
use crate::types::{FileType, Lang};

/// Compute a project fingerprint for MCP initialization.
/// Must be fast (<250ms) — runs synchronously in the initialize handler.
/// Returns empty string on any failure (no error propagation).
#[must_use]
pub fn fingerprint(root: &Path) -> String {
    let start = Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fingerprint_inner(root)));
    let elapsed = start.elapsed();
    if elapsed.as_millis() > 250 {
        eprintln!(
            "[tilth] fingerprint took {}ms (>250ms budget)",
            elapsed.as_millis()
        );
    }
    result.unwrap_or_default()
}

fn fingerprint_inner(root: &Path) -> String {
    let mut lines: Vec<String> = Vec::new();

    // Walk files (depth 2) — collect language counts, modules, entry points
    let walk = walk_files(root);

    // Determine primary language
    let primary_lang = walk
        .lang_counts
        .iter()
        .max_by_key(|(_, count)| *count)
        .map(|(lang, _)| *lang);

    let lang_name = primary_lang.map_or("Unknown", lang_display_name);
    let total_files = primary_lang
        .and_then(|l| walk.lang_counts.get(&l))
        .copied()
        .unwrap_or_else(|| walk.lang_counts.values().sum::<usize>());

    // Modules: dirs with >=2 files of the primary language, with common prefix stripped.
    // Keys in module_lang_counts may be "dir" or "dir/subdir" (for deeply nested projects).
    let modules: Vec<String> = {
        // Collect dirs with >=2 primary-language files, sorted by file count descending
        let mut mods: Vec<(String, usize)> = walk
            .module_lang_counts
            .iter()
            .filter_map(|(name, lang_map)| {
                let count = primary_lang
                    .and_then(|l| lang_map.get(&l))
                    .copied()
                    .unwrap_or(0);
                if count >= 2 {
                    Some((name.clone(), count))
                } else {
                    None
                }
            })
            .collect();
        mods.sort_by_key(|b| std::cmp::Reverse(b.1)); // most files first

        // If all modules (or at least most) share a common top-level prefix
        // (e.g., all are "src/..."), strip it so we display short names
        // ("diff/" not "src/diff/"). Also exclude the bare prefix entry itself.
        if mods.len() >= 2 {
            let prefix = common_dir_prefix(&mods);
            if !prefix.is_empty() {
                // The prefix without trailing slash (e.g., "src")
                let prefix_bare = prefix.trim_end_matches('/');
                mods = mods
                    .into_iter()
                    .filter_map(|(name, count)| {
                        if name == prefix_bare {
                            // Drop the bare prefix itself (it's the container, not a module)
                            None
                        } else if let Some(stripped) = name.strip_prefix(&prefix) {
                            let s = stripped.trim_start_matches('/');
                            if s.is_empty() {
                                None
                            } else {
                                Some((s.to_string(), count))
                            }
                        } else {
                            Some((name, count))
                        }
                    })
                    .collect();
            }
        }
        // Filter out well-known non-source directories
        let non_source = [
            "test",
            "tests",
            "__tests__",
            "spec",
            "specs",
            "doc",
            "docs",
            "docs_src",
            "documentation",
            "example",
            "examples",
            "sample",
            "samples",
            "script",
            "scripts",
            "tools",
            "fixtures",
            "benchmark",
            "benchmarks",
            "bench",
            ".github",
            ".vscode",
            ".idea",
            "vendor",
            "node_modules",
            "target",
            "dist",
            "build",
        ];
        mods.retain(|(name, _)| {
            let lower = name.to_lowercase();
            // Check if ANY path component is a non-source dir
            !lower.split('/').any(|part| non_source.contains(&part))
        });
        // Sort by file count descending, truncate to 10, extract names
        mods.sort_by_key(|b| std::cmp::Reverse(b.1));
        mods.truncate(10);
        mods.into_iter().map(|(name, _)| name).collect()
    };

    // Header line
    let dir_count = modules.len();
    lines.push(format!(
        "[tilth] {lang_name} project — {total_files} source files, {dir_count} directories"
    ));

    // Directories (cap at 10, sorted by file count descending)
    if !modules.is_empty() {
        let mut dirs = modules;
        dirs.truncate(10);
        let display: Vec<String> = dirs.iter().map(|m| format!("{m}/")).collect();
        lines.push(format!("  dirs: {}", display.join(" ")));
    }

    // Manifest — name, version, deps
    if let Some(manifest) = find_manifest(root) {
        if let Some(info) = parse_manifest(root, &manifest) {
            // Deps line
            if !info.deps.is_empty() {
                let dep_str = info.deps.join(", ");
                lines.push(format!("  deps: {dep_str}"));
            }

            // Hot files (only for projects with local imports)
            if let Some(hot) = hot_files(root, &walk, primary_lang) {
                lines.push(format!("  hot: {hot}"));
            }

            // Git context
            if let Some(git) = git_context(root) {
                lines.push(format!("  git: {git}"));
            }

            // Test style
            if let Some(tests) = test_style(root, &walk, primary_lang) {
                lines.push(format!("  tests: {tests}"));
            }

            // Manifest line
            let mut manifest_line = format!("  manifest: {manifest}");
            if let Some(name) = &info.name {
                write!(manifest_line, " ({name}").unwrap();
                if let Some(version) = &info.version {
                    write!(manifest_line, " v{version}").unwrap();
                }
                manifest_line.push(')');
            }
            lines.push(manifest_line);
        }
    } else {
        // No manifest — still show hot, git, tests
        if let Some(hot) = hot_files(root, &walk, primary_lang) {
            lines.push(format!("  hot: {hot}"));
        }
        if let Some(git) = git_context(root) {
            lines.push(format!("  git: {git}"));
        }
        if let Some(tests) = test_style(root, &walk, primary_lang) {
            lines.push(format!("  tests: {tests}"));
        }
    }

    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Common dir prefix helper
// ---------------------------------------------------------------------------

/// If all module names (which may be "a/b" style) share the same first path
/// component, return that component followed by "/". Otherwise return "".
fn common_dir_prefix(mods: &[(String, usize)]) -> String {
    if mods.is_empty() {
        return String::new();
    }
    // Extract the first path component from each name
    let first_components: Vec<&str> = mods
        .iter()
        .map(|(n, _)| n.split('/').next().unwrap_or(n))
        .collect();
    let first = first_components[0];
    if first_components.iter().all(|c| *c == first) && mods.iter().any(|(n, _)| n.contains('/')) {
        // All share the same first component and at least some have a subdir
        format!("{first}/")
    } else {
        String::new()
    }
}

// ---------------------------------------------------------------------------
// Language display
// ---------------------------------------------------------------------------

fn lang_display_name(lang: Lang) -> &'static str {
    match lang {
        Lang::Rust => "Rust",
        Lang::TypeScript => "TypeScript",
        Lang::Tsx => "TSX",
        Lang::JavaScript => "JavaScript",
        Lang::Python => "Python",
        Lang::Go => "Go",
        Lang::Java => "Java",
        Lang::Scala => "Scala",
        Lang::C => "C",
        Lang::Cpp => "C++",
        Lang::Ruby => "Ruby",
        Lang::Php => "PHP",
        Lang::Swift => "Swift",
        Lang::Kotlin => "Kotlin",
        Lang::CSharp => "C#",
        Lang::Elixir => "Elixir",
        Lang::Dockerfile => "Docker",
        Lang::Make => "Make",
    }
}

// ---------------------------------------------------------------------------
// File walk (depth 2)
// ---------------------------------------------------------------------------

struct WalkResult {
    lang_counts: HashMap<Lang, usize>,
    /// Top-level dirs → per-language file counts
    module_lang_counts: HashMap<String, HashMap<Lang, usize>>,
    /// Code files found: (path relative to root, size in bytes)
    code_files: Vec<(String, u64)>,
    /// Whether specific test dirs exist
    has_tests_dir: bool,
    has_test_dir: bool,
    has_dunder_tests: bool,
    has_spec_dir: bool,
}

fn walk_files(root: &Path) -> WalkResult {
    let mut lang_counts: HashMap<Lang, usize> = HashMap::new();
    let mut module_lang_counts: HashMap<String, HashMap<Lang, usize>> = HashMap::new();
    let mut code_files: Vec<(String, u64)> = Vec::new();
    let mut has_tests_dir = false;
    let mut has_test_dir = false;
    let mut has_dunder_tests = false;
    let mut has_spec_dir = false;

    // Walk depth 0 (root itself)
    walk_dir(
        root,
        root,
        0,
        2,
        &mut lang_counts,
        &mut module_lang_counts,
        &mut code_files,
        &mut has_tests_dir,
        &mut has_test_dir,
        &mut has_dunder_tests,
        &mut has_spec_dir,
    );

    WalkResult {
        lang_counts,
        module_lang_counts,
        code_files,
        has_tests_dir,
        has_test_dir,
        has_dunder_tests,
        has_spec_dir,
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_dir(
    dir: &Path,
    root: &Path,
    depth: usize,
    max_depth: usize,
    lang_counts: &mut HashMap<Lang, usize>,
    module_lang_counts: &mut HashMap<String, HashMap<Lang, usize>>,
    code_files: &mut Vec<(String, u64)>,
    has_tests_dir: &mut bool,
    has_test_dir: &mut bool,
    has_dunder_tests: &mut bool,
    has_spec_dir: &mut bool,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        let Ok(ft) = entry.file_type() else {
            continue;
        };

        if ft.is_dir() {
            if SKIP_DIRS.contains(&name) {
                continue;
            }

            // Track test directories at any depth
            match name {
                "tests" => *has_tests_dir = true,
                "test" => *has_test_dir = true,
                "__tests__" => *has_dunder_tests = true,
                "spec" => *has_spec_dir = true,
                _ => {}
            }

            if depth < max_depth {
                walk_dir(
                    &path,
                    root,
                    depth + 1,
                    max_depth,
                    lang_counts,
                    module_lang_counts,
                    code_files,
                    has_tests_dir,
                    has_test_dir,
                    has_dunder_tests,
                    has_spec_dir,
                );
            }
        } else if ft.is_file() {
            if let FileType::Code(lang) = detect_file_type(&path) {
                *lang_counts.entry(lang).or_insert(0) += 1;

                // Track size for hot files
                let size = entry.metadata().map_or(0, |m| m.len());
                if let Ok(rel) = path.strip_prefix(root) {
                    let rel_str = rel.to_string_lossy().to_string();

                    code_files.push((rel_str, size));

                    // Track module — use up to 2 path components as the key,
                    // but only for files nested at least one level deep.
                    // e.g. src/diff/mod.rs → key "src/diff", lib.rs → skipped
                    {
                        let mut comps = rel.components();
                        if let Some(c1) = comps.next() {
                            let remaining: Vec<_> = comps.collect();
                            if !remaining.is_empty() {
                                let key = if remaining.len() >= 2 {
                                    // File is at depth 3+: use first two components
                                    format!(
                                        "{}/{}",
                                        c1.as_os_str().to_string_lossy(),
                                        remaining[0].as_os_str().to_string_lossy()
                                    )
                                } else {
                                    // File is at depth 2: use first component only
                                    c1.as_os_str().to_string_lossy().to_string()
                                };
                                *module_lang_counts
                                    .entry(key)
                                    .or_default()
                                    .entry(lang)
                                    .or_insert(0) += 1;
                            }
                        }
                    }
                }
            }

            // Check test file patterns
            if name.contains(".test.") || name.contains(".spec.") {
                // These contribute to test style but we detect in test_style()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Manifest parsing
// ---------------------------------------------------------------------------

fn find_manifest(root: &Path) -> Option<String> {
    const MANIFESTS: &[&str] = &["Cargo.toml", "package.json", "go.mod", "pyproject.toml"];
    for m in MANIFESTS {
        if root.join(m).exists() {
            return Some((*m).to_string());
        }
    }
    None
}

struct ManifestInfo {
    name: Option<String>,
    version: Option<String>,
    deps: Vec<String>,
}

fn parse_manifest(root: &Path, manifest: &str) -> Option<ManifestInfo> {
    match manifest {
        "Cargo.toml" => parse_cargo_toml(root),
        "package.json" => parse_package_json(root),
        "go.mod" => parse_go_mod(root),
        "pyproject.toml" => parse_pyproject_toml(root),
        _ => None,
    }
}

fn parse_cargo_toml(root: &Path) -> Option<ManifestInfo> {
    let content = fs::read_to_string(root.join("Cargo.toml")).ok()?;
    let mut name = None;
    let mut version = None;
    let mut deps: Vec<String> = Vec::new();
    let mut in_package = false;
    let mut in_deps = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            in_deps = trimmed == "[dependencies]";
            continue;
        }

        if in_package {
            if let Some(val) = extract_toml_string_value(trimmed, "name") {
                name = Some(val);
            } else if let Some(val) = extract_toml_string_value(trimmed, "version") {
                version = Some(val);
            }
        }

        if in_deps {
            // dep_name = "version" or dep_name = { version = "..." }
            if let Some(dep_name) = trimmed.split('=').next() {
                let dep = dep_name.trim();
                if !dep.is_empty() && !dep.starts_with('#') {
                    deps.push(dep.to_string());
                }
            }
        }
    }

    deps.sort();
    deps.truncate(10);

    Some(ManifestInfo {
        name,
        version,
        deps,
    })
}

fn parse_package_json(root: &Path) -> Option<ManifestInfo> {
    let content = fs::read_to_string(root.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    let name = json.get("name").and_then(|v| v.as_str()).map(String::from);
    let version = json
        .get("version")
        .and_then(|v| v.as_str())
        .map(String::from);

    let mut deps: Vec<String> = Vec::new();
    if let Some(obj) = json.get("dependencies").and_then(|v| v.as_object()) {
        for key in obj.keys() {
            deps.push(key.clone());
        }
    }
    deps.sort();
    deps.truncate(10);

    Some(ManifestInfo {
        name,
        version,
        deps,
    })
}

fn parse_go_mod(root: &Path) -> Option<ManifestInfo> {
    let content = fs::read_to_string(root.join("go.mod")).ok()?;
    let mut name = None;
    let mut deps: Vec<String> = Vec::new();
    let mut in_require = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("module ") {
            name = Some(rest.trim().to_string());
        }
        if trimmed == "require (" {
            in_require = true;
            continue;
        }
        if trimmed == ")" {
            in_require = false;
            continue;
        }
        if in_require {
            // e.g. "github.com/gin-gonic/gin v1.9.0"
            if let Some(dep) = trimmed.split_whitespace().next() {
                if !dep.starts_with("//") {
                    // Use short name (last segment of module path)
                    let short = dep.rsplit('/').next().unwrap_or(dep);
                    deps.push(short.to_string());
                }
            }
        }
    }

    deps.sort();
    deps.truncate(10);

    Some(ManifestInfo {
        name,
        version: None,
        deps,
    })
}

fn parse_pyproject_toml(root: &Path) -> Option<ManifestInfo> {
    let content = fs::read_to_string(root.join("pyproject.toml")).ok()?;
    let mut name = None;
    let mut version = None;
    let mut deps: Vec<String> = Vec::new();
    let mut in_project = false;
    let mut in_deps = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') {
            in_project = trimmed == "[project]";
            in_deps = trimmed == "[project.dependencies]"
                || (in_project && trimmed == "dependencies = [");
            continue;
        }

        if in_project {
            if let Some(val) = extract_toml_string_value(trimmed, "name") {
                name = Some(val);
            } else if let Some(val) = extract_toml_string_value(trimmed, "version") {
                version = Some(val);
            }

            // Inline dependencies array
            if trimmed.starts_with("dependencies") && trimmed.contains('[') {
                // Parse inline: dependencies = ["dep1", "dep2>=1.0"]
                if let Some(arr_start) = trimmed.find('[') {
                    let arr_content = &trimmed[arr_start..];
                    for item in arr_content.split('"') {
                        let item = item.trim();
                        if item.is_empty()
                            || item.starts_with('[')
                            || item.starts_with(']')
                            || item.starts_with(',')
                        {
                            continue;
                        }
                        // Extract package name (before any version specifier)
                        let dep_name = item
                            .split(&['>', '<', '=', '~', '!', ';', '['][..])
                            .next()
                            .unwrap_or(item)
                            .trim();
                        if !dep_name.is_empty() {
                            deps.push(dep_name.to_string());
                        }
                    }
                }
            }
        }

        if in_deps && !trimmed.starts_with('[') {
            // Multi-line deps array items: "dep_name>=1.0",
            let clean = trimmed.trim_matches(&['"', '\'', ',', ' '][..]);
            if !clean.is_empty() && clean != "]" {
                let dep_name = clean
                    .split(&['>', '<', '=', '~', '!', ';', '['][..])
                    .next()
                    .unwrap_or(clean)
                    .trim();
                if !dep_name.is_empty() {
                    deps.push(dep_name.to_string());
                }
            }
        }
    }

    deps.sort();
    deps.truncate(10);

    Some(ManifestInfo {
        name,
        version,
        deps,
    })
}

/// Extract a string value from a TOML key = "value" line.
fn extract_toml_string_value(line: &str, key: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with(key) {
        return None;
    }
    let rest = trimmed[key.len()..].trim_start();
    if !rest.starts_with('=') {
        return None;
    }
    let after_eq = rest[1..].trim();
    // Extract value between quotes: "value" or 'value'
    let val = if let Some(rest) = after_eq.strip_prefix('"') {
        rest.split('"').next().unwrap_or("")
    } else if let Some(rest) = after_eq.strip_prefix('\'') {
        rest.split('\'').next().unwrap_or("")
    } else {
        // Bare value — take up to whitespace or comment
        after_eq.split_whitespace().next().unwrap_or("")
    };
    if val.is_empty() {
        return None;
    }
    Some(val.to_string())
}

// ---------------------------------------------------------------------------
// Git context
// ---------------------------------------------------------------------------

/// Run a git command with a 200ms timeout. Returns None if it fails or times out.
fn git_output(root: &Path, args: &[&str]) -> Option<String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + Duration::from_millis(200);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let out = child.stdout.take()?;
                let s = std::io::read_to_string(out).ok()?;
                let trimmed = s.trim().to_string();
                return if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return None,
        }
    }
}

fn git_context(root: &Path) -> Option<String> {
    let branch = git_output(root, &["branch", "--show-current"])
        .or_else(|| git_output(root, &["rev-parse", "--short", "HEAD"]))?;

    let dirty_count = git_output(root, &["status", "--porcelain"]).map_or(0, |s| s.lines().count());

    let dirty_str = if dirty_count == 0 {
        "clean".to_string()
    } else {
        format!("{dirty_count} uncommitted files")
    };

    Some(format!("branch {branch}, {dirty_str}"))
}

// ---------------------------------------------------------------------------
// Test style detection
// ---------------------------------------------------------------------------

fn test_style(root: &Path, walk: &WalkResult, primary_lang: Option<Lang>) -> Option<String> {
    let mut styles: Vec<String> = Vec::new();

    // Directory-based test detection
    if walk.has_tests_dir {
        styles.push("tests/".to_string());
    }
    if walk.has_test_dir {
        styles.push("test/".to_string());
    }
    if walk.has_dunder_tests {
        styles.push("__tests__/".to_string());
    }
    if walk.has_spec_dir {
        styles.push("spec/".to_string());
    }

    // File pattern detection
    let has_test_files = walk
        .code_files
        .iter()
        .any(|(path, _)| path.contains(".test.") || path.contains(".spec."));
    let has_go_tests = walk
        .code_files
        .iter()
        .any(|(path, _)| path.ends_with("_test.go"));
    let has_py_tests = walk
        .code_files
        .iter()
        .any(|(path, _)| path.starts_with("test_") || path.contains("/test_"));

    if has_test_files && !walk.has_dunder_tests {
        styles.push("*.test/spec files".to_string());
    }
    if has_go_tests {
        styles.push("_test.go".to_string());
    }
    if has_py_tests {
        styles.push("test_*.py".to_string());
    }

    // Rust in-source test detection
    if primary_lang == Some(Lang::Rust) {
        let has_cfg_test = walk
            .code_files
            .iter()
            .filter(|(path, _)| {
                Path::new(path)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
            })
            .take(5)
            .any(|(path, _)| {
                let full = root.join(path);
                fs::read_to_string(&full)
                    .ok()
                    .is_some_and(|content| content.contains("#[cfg(test)]"))
            });
        if has_cfg_test {
            styles.push("in-source #[cfg(test)]".to_string());
        }
    }

    if styles.is_empty() {
        None
    } else {
        Some(styles.join(", "))
    }
}

// ---------------------------------------------------------------------------
// Hot files — most imported local files
// ---------------------------------------------------------------------------

fn hot_files(root: &Path, walk: &WalkResult, primary_lang: Option<Lang>) -> Option<String> {
    let lang = primary_lang?; // require a detected language
    let start = Instant::now();

    // Sort by size (smallest first) and take first 100
    let mut files: Vec<&(String, u64)> = walk.code_files.iter().collect();
    files.sort_by_key(|(_, size)| *size);
    files.truncate(100);

    // Use resolve_related_files to get real file paths for imports.
    // Count how many files import each target path.
    let mut path_counts: HashMap<std::path::PathBuf, usize> = HashMap::new();
    // Also collect all import source lines for symbol extraction later
    let mut all_import_sources: Vec<String> = Vec::new();

    for (rel_path, _) in &files {
        if start.elapsed().as_millis() > 100 {
            break;
        }
        let full = root.join(rel_path);
        let Ok(content) = fs::read_to_string(&full) else {
            continue;
        };

        // Resolve imports to actual file paths using the proven import resolver
        let resolved = crate::read::imports::resolve_related_files_with_content(&full, &content);
        for target_path in resolved {
            *path_counts.entry(target_path).or_insert(0) += 1;
        }

        // Collect import source strings for symbol extraction
        for line in content.lines() {
            if is_import_line(line, lang) {
                let source = crate::lang::outline::extract_import_source(line, Some(lang));
                if !source.is_empty() && !crate::read::imports::is_external(&source, lang) {
                    all_import_sources.push(source);
                }
            }
        }
    }

    if path_counts.is_empty() {
        return None;
    }

    // Sort by import count descending, take top 5
    let mut sorted: Vec<(std::path::PathBuf, usize)> = path_counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    sorted.truncate(5);

    if sorted[0].1 < 2 {
        return None;
    }

    // For each hot file, find the most commonly imported symbol by scanning
    // import sources that reference this file's module name.
    let parts: Vec<String> = sorted
        .iter()
        .filter(|(_, count)| *count >= 2)
        .map(|(path, count)| {
            let rel = path.strip_prefix(root).unwrap_or(path);
            let rel_str = rel.display().to_string();

            // Derive the module name from the file path
            // src/types.rs → "types", src/lang/mod.rs → "lang", src/error.rs → "error"
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let module_name = if stem == "mod" || stem == "index" || stem == "__init__" {
                path.parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or(stem)
            } else {
                stem
            };

            // Count symbols imported from this module across all import sources
            let mut symbol_counts: HashMap<String, usize> = HashMap::new();
            for source in &all_import_sources {
                // Match imports that reference this module
                // e.g., for module "types": "crate::types::OutlineEntry" matches
                let segments: Vec<&str> = source.split("::").collect();
                if let Some(mod_pos) = segments.iter().position(|s| *s == module_name) {
                    // Everything after the module name is a symbol path
                    for &sym in segments.iter().skip(mod_pos + 1) {
                        if !sym.is_empty()
                            && !sym.contains('*')
                            && !sym.contains('{')
                            && sym != "self"
                        {
                            *symbol_counts.entry(sym.to_string()).or_insert(0) += 1;
                        }
                    }
                }
            }

            // Pick the most frequently imported symbol (break ties alphabetically for determinism)
            let top_sym = symbol_counts
                .into_iter()
                .max_by(|(a_sym, a_c), (b_sym, b_c)| a_c.cmp(b_c).then(b_sym.cmp(a_sym)))
                .map(|(sym, _)| sym);

            if let Some(sym) = top_sym {
                format!("{rel_str}({sym}) ×{count}")
            } else {
                format!("{rel_str} ×{count}")
            }
        })
        .collect();

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fingerprint_on_tilth() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let output = fingerprint(root);

        assert!(!output.is_empty(), "fingerprint should not be empty");
        assert!(
            output.contains("Rust"),
            "should detect Rust as primary language"
        );
        assert!(output.contains("Cargo.toml"), "should detect manifest");
        assert!(output.contains("tilth"), "should find project name");

        // Token budget: output should be compact
        let estimated_tokens = output.len() / 4;
        assert!(
            estimated_tokens < 300,
            "fingerprint should be <300 tokens, got {estimated_tokens}"
        );
    }

    #[test]
    fn test_fingerprint_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let output = fingerprint(tmp.path());

        // Empty dir: should produce minimal output or empty
        // With 0 files and 0 modules, the header will say "0 source files"
        // but that's fine — it's still useful context
        assert!(
            output.is_empty() || output.contains("0 source files"),
            "empty dir should produce empty or minimal output, got: {output}"
        );
    }

    #[test]
    fn test_manifest_parsing() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let info = parse_cargo_toml(root).expect("should parse Cargo.toml");

        assert_eq!(info.name.as_deref(), Some("tilth"));
        assert!(info.version.is_some(), "should have a version");
        assert!(
            info.deps.iter().any(|d| d == "clap"),
            "deps should include clap: {:?}",
            info.deps
        );
        assert!(
            info.deps.iter().any(|d| d == "dashmap"),
            "deps should include dashmap: {:?}",
            info.deps
        );
    }
}
