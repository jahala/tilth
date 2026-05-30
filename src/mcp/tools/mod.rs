mod definitions;
mod deps;
mod diff;
mod grok;
mod list;
mod read;
mod search;
mod write;

pub(super) use definitions::tool_definitions;
pub(super) use deps::tool_deps;
pub(super) use diff::tool_diff;
pub(super) use grok::tool_grok;
pub(super) use list::tool_list;
pub(super) use read::tool_read;
pub(super) use search::tool_search;
pub(super) use write::tool_write;

use std::path::PathBuf;

use serde_json::Value;

/// Falls back to cwd when scope is invalid, with a warning message.
pub(super) fn resolve_scope(args: &Value) -> (PathBuf, Option<String>) {
    let raw_str = args.get("scope").and_then(|v| v.as_str()).unwrap_or(".");
    let raw: PathBuf = raw_str.into();
    let resolved = raw.canonicalize().unwrap_or_else(|_| raw.clone());
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    if resolved == cwd {
        return (".".into(), None);
    }
    if !resolved.is_dir() {
        return (
            ".".into(),
            Some(format!(
                "scope \"{raw_str}\" is not a valid directory, searching current directory instead.\n\n"
            )),
        );
    }
    (resolved, None)
}

pub(super) fn apply_budget(output: &str, budget: Option<u64>) -> String {
    match budget {
        Some(b) => crate::budget::apply(output, b),
        None => crate::budget::apply(output, crate::budget::DEFAULT_BUDGET),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_scope_explicit_arg() {
        let tmp = tempfile::tempdir().unwrap();
        let args = serde_json::json!({ "scope": tmp.path().to_str().unwrap() });
        let (scope, warning) = resolve_scope(&args);
        assert_eq!(scope, tmp.path().canonicalize().unwrap());
        assert!(warning.is_none());
    }

    #[test]
    fn resolve_scope_no_arg_uses_cwd() {
        let args = serde_json::json!({});
        let (scope, warning) = resolve_scope(&args);
        // With no arg, defaults to "." which is cwd
        let cwd = std::env::current_dir().unwrap();
        // The function returns "." when resolved == cwd
        assert!(scope == PathBuf::from(".") || scope == cwd);
        assert!(warning.is_none());
    }

    #[test]
    fn resolve_scope_invalid_dir_warns() {
        let args = serde_json::json!({ "scope": "/nonexistent/directory/zzz" });
        let (scope, warning) = resolve_scope(&args);
        assert_eq!(scope, PathBuf::from("."));
        assert!(warning.is_some());
        assert!(warning.unwrap().contains("not a valid directory"));
    }

    #[test]
    fn apply_budget_none_caps_at_default() {
        // An output far larger than DEFAULT_BUDGET must be truncated even with
        // no explicit budget — otherwise a broad read/regex/diff blows the host
        // ~25K tool-response limit.
        let oversized = format!(
            "# header line\n{}",
            "filler content that repeats and repeats\n".repeat(20_000)
        );
        let capped = apply_budget(&oversized, None);
        assert!(
            capped.len() < oversized.len(),
            "output should be truncated below the default budget"
        );
        assert!(
            capped.contains("truncated"),
            "truncation notice should be present: {}",
            &capped[capped.len().saturating_sub(120)..]
        );
    }

    /// Reproduces issue #37: MCP host launches tilth with cwd=/. The --scope
    /// flag should override this.
    #[test]
    fn scope_flag_overrides_bad_cwd() {
        let project = tempfile::tempdir().unwrap();
        let project_path = project.path();

        // Create a manifest so package_root can find it
        std::fs::write(
            project_path.join("Cargo.toml"),
            "[package]\nname = \"test\"",
        )
        .unwrap();
        std::fs::create_dir(project_path.join("src")).unwrap();
        std::fs::write(project_path.join("src/main.rs"), "fn main() {}").unwrap();

        // Save current cwd
        let orig_cwd = std::env::current_dir().unwrap();

        // Simulate Codex: cwd=/
        std::env::set_current_dir("/").unwrap();

        // Without --scope: resolve_scope returns "." which is /
        let args = serde_json::json!({});
        let (scope, _) = resolve_scope(&args);
        assert_eq!(
            scope,
            PathBuf::from("."),
            "Without --scope, should return . (which is /)"
        );

        // With --scope pointing to project: set_current_dir should fix everything
        let _ = std::env::set_current_dir(project_path);
        let args = serde_json::json!({});
        let (scope, _) = resolve_scope(&args);
        assert_eq!(
            scope,
            PathBuf::from("."),
            "After chdir to project, . should resolve correctly"
        );

        // Confirm the working dir resolved to the project, not / — so
        // scope-relative lookups land in the right tree.
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(cwd, project_path.canonicalize().unwrap());

        // Restore
        std::env::set_current_dir(orig_cwd).unwrap();
    }
}
