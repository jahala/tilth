//! The shared directory walker: one policy for which files every search,
//! callers walk and map visits.

use std::num::NonZeroUsize;
use std::path::Path;

use ignore::WalkBuilder;

use crate::error::TilthError;

// Directories that are always skipped — build artifacts, dependencies, VCS internals.
// Keep this list even when ignore files are disabled; it covers common
// high-volume directories that are rarely useful search targets.
pub const SKIP_DIRS: &[&str] = &[
    ".git",
    ".jj",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "dist",
    "build",
    "__pycache__",
    ".pycache",
    "vendor",
    ".next",
    ".nuxt",
    "coverage",
    ".cache",
    ".tox",
    ".venv",
    ".eggs",
    ".mypy_cache",
    ".ruff_cache",
    ".pytest_cache",
    ".turbo",
    ".parcel-cache",
    ".svelte-kit",
    "out",
    ".output",
    ".vercel",
    ".netlify",
    ".gradle",
    ".idea",
    ".scala-build",
    ".bloop",
    ".metals",
];

thread_local! {
    static CALL_GITIGNORE_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

pub fn call_gitignore_override() -> Option<bool> {
    CALL_GITIGNORE_OVERRIDE.with(std::cell::Cell::get)
}

pub fn with_gitignore_override<T>(value: Option<bool>, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<bool>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CALL_GITIGNORE_OVERRIDE.with(|current| current.set(self.0));
        }
    }
    let previous = CALL_GITIGNORE_OVERRIDE.with(|current| current.replace(value));
    let _restore = Restore(previous);
    f()
}

/// How walks run. The binary builds it once at startup and passes it to
/// every walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalkConfig {
    /// Worker threads for a parallel walk. `None` lets the walker choose.
    pub threads: Option<NonZeroUsize>,
    /// Whether walks honor `.gitignore`, `.ignore` and Git exclude rules.
    /// A per-call MCP override, when one is set, wins over it.
    pub respect_gitignore: bool,
}

impl WalkConfig {
    /// Half the available cores, kept between 2 and 6, or 4 when the core
    /// count is unknown.
    #[must_use]
    pub fn default_threads() -> NonZeroUsize {
        const TWO: NonZeroUsize = NonZeroUsize::MIN.saturating_add(1);
        const FOUR: NonZeroUsize = NonZeroUsize::MIN.saturating_add(3);
        const SIX: NonZeroUsize = NonZeroUsize::MIN.saturating_add(5);
        std::thread::available_parallelism().map_or(FOUR, |cores| {
            NonZeroUsize::new(cores.get() / 2).map_or(TWO, |half| half.clamp(TWO, SIX))
        })
    }

    /// Whether this walk honors gitignore-style files: the per-call MCP
    /// override when one is set, else [`respect_gitignore`](Self::respect_gitignore).
    fn honors_gitignore(self) -> bool {
        call_gitignore_override().unwrap_or(self.respect_gitignore)
    }
}

impl Default for WalkConfig {
    /// [`default_threads`](Self::default_threads) workers, gitignore off.
    fn default() -> Self {
        Self {
            threads: Some(Self::default_threads()),
            respect_gitignore: false,
        }
    }
}

/// Shared walker policy: searches files except known junk directories and
/// `.tilthignore` entries. Gitignore-style files are opt-in.
/// Parent `.tilthignore` files apply at every level above the scope.
/// The scope itself is an explicit traversal root: `--scope src/tmp` searches
/// that directory even if a parent `.gitignore` excludes `src/tmp/`; ignore
/// rules still filter entries beneath the scope.
/// Used by both the parallel search walker (`walker()`) and the sequential map
/// walker (the `tilth` binary's `map::generate`), which each apply their own final
/// `.max_depth()`/`.threads()` and `.build()`/`.build_parallel()`.
#[must_use]
pub fn base_walk_builder(scope: &Path, walk: WalkConfig) -> WalkBuilder {
    base_walk_builder_with_gitignore(scope, walk.honors_gitignore())
}

fn base_walk_builder_with_gitignore(scope: &Path, respect_gitignore: bool) -> WalkBuilder {
    let mut builder = WalkBuilder::new(scope);
    builder
        .follow_links(true)
        .same_file_system(true) // Stop at mount boundaries (NFS, external volumes).
        .hidden(false)
        .git_ignore(respect_gitignore)
        .git_global(respect_gitignore)
        .git_exclude(respect_gitignore)
        .ignore(respect_gitignore)
        .require_git(!respect_gitignore)
        .parents(true)
        .add_custom_ignore_filename(".tilthignore")
        .filter_entry(include_entry);
    builder
}

fn include_entry(entry: &ignore::DirEntry) -> bool {
    if entry.file_type().is_some_and(|ft| ft.is_dir()) {
        if let Some(name) = entry.file_name().to_str() {
            return !SKIP_DIRS.contains(&name);
        }
    }
    true
}

/// Build a parallel directory walker over `.tilthignore`-filtered files except
/// known junk directories. Gitignore-style files are opt-in.
/// When `glob` is Some, applies a file-pattern filter (whitelist or negation).
/// With gitignore enabled, the glob cannot override ignore rules.
pub fn walker(
    scope: &Path,
    glob: Option<&str>,
    walk: WalkConfig,
) -> Result<ignore::WalkParallel, TilthError> {
    let respect_gitignore = walk.honors_gitignore();
    let mut builder = base_walk_builder_with_gitignore(scope, respect_gitignore);
    // The walker reads 0 as "choose for me".
    builder.threads(walk.threads.map_or(0, NonZeroUsize::get));

    if let Some(pattern) = glob {
        if !pattern.is_empty() {
            let mut overrides = ignore::overrides::OverrideBuilder::new(scope);
            overrides
                .add(pattern)
                .map_err(|e| TilthError::InvalidQuery {
                    query: pattern.to_string(),
                    reason: format!("invalid glob: {e}"),
                })?;
            let overrides = overrides.build().map_err(|e| TilthError::InvalidQuery {
                query: pattern.to_string(),
                reason: format!("invalid glob: {e}"),
            })?;
            if respect_gitignore {
                // A glob narrows the walk without whitelisting ignored entries.
                builder.filter_entry(move |entry| {
                    include_entry(entry)
                        && !overrides
                            .matched(
                                entry.path(),
                                entry.file_type().is_some_and(|ft| ft.is_dir()),
                            )
                            .is_ignore()
                });
            } else {
                builder.overrides(overrides);
            }
        }
    }

    Ok(builder.build_parallel())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// Collect all file paths from a walker into a sorted Vec.
    fn walk_paths(scope: &Path, glob: Option<&str>) -> Vec<PathBuf> {
        let w = walker(scope, glob, WalkConfig::default()).expect("walker failed");
        collect_walk_paths(w)
    }

    fn walk_paths_with_gitignore(scope: &Path, respect_gitignore: bool) -> Vec<PathBuf> {
        let w = base_walk_builder_with_gitignore(scope, respect_gitignore).build_parallel();
        collect_walk_paths(w)
    }

    fn collect_walk_paths(w: ignore::WalkParallel) -> Vec<PathBuf> {
        let paths: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
        w.run(|| {
            let paths = &paths;
            Box::new(move |entry| {
                if let Ok(e) = entry {
                    if e.file_type().is_some_and(|ft| ft.is_file()) {
                        paths.lock().unwrap().push(e.into_path());
                    }
                }
                ignore::WalkState::Continue
            })
        });
        let mut v = paths.into_inner().unwrap();
        v.sort();
        v
    }

    fn extensions(paths: &[PathBuf]) -> HashSet<String> {
        paths
            .iter()
            .filter_map(|p| p.extension())
            .map(|e| e.to_string_lossy().to_string())
            .collect()
    }

    // ── walker unit tests ──

    #[test]
    fn walker_none_returns_all_file_types() {
        let scope = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let all = walk_paths(&scope, None);
        let exts = extensions(&all);
        assert!(exts.contains("rs"), "expected .rs files, got {exts:?}");
        assert_ne!(all.len(), 0, "no files walked under {}", scope.display());
    }

    #[test]
    fn walker_gitignore_setting_controls_ignored_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        std::fs::create_dir(tmp.path().join("ignored")).unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "ignored/\n").unwrap();
        std::fs::write(tmp.path().join("visible.rs"), "fn visible() {}\n").unwrap();
        std::fs::write(
            tmp.path().join("ignored").join("hidden.rs"),
            "fn hidden() {}\n",
        )
        .unwrap();

        for enabled in [false, true] {
            let rels: HashSet<String> = walk_paths_with_gitignore(tmp.path(), enabled)
                .iter()
                .filter_map(|p| p.strip_prefix(tmp.path()).ok())
                .map(|p| p.to_string_lossy().to_string())
                .collect();
            assert!(
                rels.contains("visible.rs"),
                "visible file missing: {rels:?}"
            );
            assert_eq!(
                rels.contains("ignored/hidden.rs"),
                !enabled,
                "unexpected gitignore behavior with enabled={enabled}: {rels:?}"
            );
        }
    }

    #[test]
    fn walker_respects_tilthignore_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("ignored")).unwrap();
        std::fs::write(tmp.path().join(".tilthignore"), "ignored/\n").unwrap();
        std::fs::write(tmp.path().join("visible.rs"), "fn visible() {}\n").unwrap();
        std::fs::write(
            tmp.path().join("ignored").join("hidden.rs"),
            "fn hidden() {}\n",
        )
        .unwrap();

        let paths = walk_paths_with_gitignore(tmp.path(), false);
        let rels: HashSet<String> = paths
            .iter()
            .filter_map(|p| p.strip_prefix(tmp.path()).ok())
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        assert!(
            rels.contains("visible.rs"),
            "non-ignored file should be walked: {rels:?}"
        );
        assert!(
            !rels.contains("ignored/hidden.rs"),
            ".tilthignore file should not be walked: {rels:?}"
        );
    }

    #[test]
    fn walker_respects_parent_tilthignore_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let ignored = src.join("ignored");
        std::fs::create_dir_all(&ignored).unwrap();
        std::fs::write(tmp.path().join(".tilthignore"), "src/ignored/\n").unwrap();
        std::fs::write(src.join("visible.rs"), "fn visible() {}\n").unwrap();
        std::fs::write(ignored.join("hidden.rs"), "fn hidden() {}\n").unwrap();

        let paths = walk_paths_with_gitignore(&src, false);
        let rels: HashSet<String> = paths
            .iter()
            .filter_map(|p| p.strip_prefix(&src).ok())
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        assert!(
            rels.contains("visible.rs"),
            "non-ignored file should be walked: {rels:?}"
        );
        assert!(
            !rels.contains("ignored/hidden.rs"),
            "parent .tilthignore file should not be walked: {rels:?}"
        );
    }

    #[test]
    fn walker_skips_vcs_internal_dirs() {
        // `.jj`/`.hg`/`.svn` are VCS internals like `.git` — never source.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("real.rs"), "fn main() {}").unwrap();
        for vcs in [".jj", ".hg", ".svn"] {
            let dir = tmp.path().join(vcs);
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("store.rs"), "fn buried() {}").unwrap();
        }

        let names: Vec<String> = walk_paths(tmp.path(), None)
            .iter()
            .filter_map(|p| Some(p.file_name()?.to_str()?.to_string()))
            .collect();
        assert!(
            names.contains(&"real.rs".to_string()),
            "source file must be found: {names:?}"
        );
        assert!(
            !names.contains(&"store.rs".to_string()),
            "VCS-internal file leaked through the walker: {names:?}"
        );
    }

    #[test]
    fn walker_whitelist_filters_to_matching_extension() {
        let scope = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let filtered = walk_paths(&scope, Some("*.rs"));
        assert!(!filtered.is_empty(), "whitelist should find .rs files");
        for p in &filtered {
            assert_eq!(
                p.extension().and_then(|e| e.to_str()),
                Some("rs"),
                "non-.rs file leaked through whitelist: {}",
                p.display()
            );
        }
    }

    #[test]
    fn walker_negation_excludes_matching_extension() {
        let scope = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let without_rs = walk_paths(&scope, Some("!*.rs"));
        for p in &without_rs {
            assert_ne!(
                p.extension().and_then(|e| e.to_str()),
                Some("rs"),
                ".rs file leaked through negation: {}",
                p.display()
            );
        }
    }

    #[test]
    fn walker_empty_string_equals_none() {
        let scope = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let all = walk_paths(&scope, None);
        let empty = walk_paths(&scope, Some(""));
        assert_eq!(all.len(), empty.len(), "empty glob should behave like None");
    }

    #[test]
    fn walker_invalid_glob_returns_error() {
        let scope = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let result = walker(&scope, Some("[unclosed"), WalkConfig::default());
        match result {
            Err(TilthError::InvalidQuery { query, reason }) => {
                assert_eq!(query, "[unclosed");
                assert!(
                    reason.contains("invalid glob"),
                    "reason should mention 'invalid glob': {reason}"
                );
            }
            Err(other) => panic!("expected InvalidQuery, got {other}"),
            Ok(_) => panic!("expected Err for invalid glob, got Ok"),
        }
    }

    #[test]
    fn walker_brace_expansion_matches_multiple_extensions() {
        let scope = Path::new(env!("CARGO_MANIFEST_DIR"));
        let filtered = walk_paths(scope, Some("*.{rs,toml}"));
        let exts = extensions(&filtered);
        assert!(
            exts.contains("rs"),
            "brace expansion should include .rs: {exts:?}"
        );
        assert!(
            exts.contains("toml"),
            "brace expansion should include .toml: {exts:?}"
        );
        for ext in &exts {
            assert!(
                ext == "rs" || ext == "toml",
                "unexpected extension leaked: {ext}"
            );
        }
    }

    #[test]
    fn walker_whitelist_fewer_than_unfiltered() {
        // Use project root (not src/) — project root has .toml, .md, .lock etc.
        // alongside .rs files, so *.rs is guaranteed to be a strict subset.
        let scope = Path::new(env!("CARGO_MANIFEST_DIR"));
        let all = walk_paths(scope, None);
        let rs_only = walk_paths(scope, Some("*.rs"));
        assert!(
            rs_only.len() < all.len(),
            "whitelist ({}) should find fewer files than unfiltered ({})",
            rs_only.len(),
            all.len()
        );
    }

    #[test]
    fn walker_path_pattern_restricts_directory() {
        let scope = Path::new(env!("CARGO_MANIFEST_DIR"));
        let filtered = walk_paths(scope, Some("src/**/*.rs"));
        assert!(!filtered.is_empty(), "path pattern should find files");
        let src_dir = scope.join("src");
        for p in &filtered {
            assert!(
                p.starts_with(&src_dir),
                "file outside src/ leaked: {}",
                p.display()
            );
        }
    }

    #[test]
    fn walker_follows_symlinked_file() {
        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().join("real");
        std::fs::create_dir(&real_dir).unwrap();
        std::fs::write(real_dir.join("hello.rs"), "fn main() {}").unwrap();

        let link_dir = tmp.path().join("linked");
        std::fs::create_dir(&link_dir).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(real_dir.join("hello.rs"), link_dir.join("hello.rs")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(real_dir.join("hello.rs"), link_dir.join("hello.rs"))
            .unwrap();

        let paths = walk_paths(tmp.path(), None);
        let names: Vec<&str> = paths
            .iter()
            .filter_map(|p| p.file_name()?.to_str())
            .collect();
        // Should find hello.rs twice: once in real/, once via the symlink in linked/
        assert_eq!(
            names.iter().filter(|n| **n == "hello.rs").count(),
            2,
            "expected hello.rs from both real and symlinked dirs, got: {names:?}"
        );
    }

    #[test]
    fn walker_follows_symlinked_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().join("real_pkg");
        std::fs::create_dir(&real_dir).unwrap();
        std::fs::write(real_dir.join("lib.rs"), "pub fn add() {}").unwrap();
        std::fs::write(real_dir.join("util.rs"), "pub fn helper() {}").unwrap();

        // Symlink the entire directory
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_dir, tmp.path().join("deps_link")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&real_dir, tmp.path().join("deps_link")).unwrap();

        let paths = walk_paths(tmp.path(), None);
        let link_files: Vec<_> = paths
            .iter()
            .filter(|p| p.starts_with(tmp.path().join("deps_link")))
            .collect();
        assert_eq!(
            link_files.len(),
            2,
            "expected 2 files via symlinked directory, got: {link_files:?}"
        );
    }

    #[test]
    fn walker_survives_symlink_cycle() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("real.rs"), "fn main() {}").unwrap();

        // Create a symlink cycle: loop -> .
        #[cfg(unix)]
        std::os::unix::fs::symlink(tmp.path(), tmp.path().join("loop")).unwrap();

        // Should complete without hanging — ignore crate detects the cycle via inode tracking
        let paths = walk_paths(tmp.path(), None);
        let names: Vec<&str> = paths
            .iter()
            .filter_map(|p| p.file_name()?.to_str())
            .collect();
        assert!(
            names.contains(&"real.rs"),
            "should find real.rs despite cycle: {names:?}"
        );
    }
}
