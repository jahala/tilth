mod definitions;
mod deps;
mod diff;
mod grok;
mod list;
mod read;
mod savings;
mod search;
mod session;
mod write;

pub(super) use definitions::tool_definitions;
pub(super) use deps::tool_deps;
pub(super) use diff::tool_diff;
pub(super) use grok::tool_grok;
pub(super) use list::tool_list;
pub(super) use read::tool_read;
pub(super) use savings::tool_savings;
pub(super) use search::tool_search;
pub(super) use session::tool_session;
pub(super) use write::tool_write;

use std::path::PathBuf;

use serde_json::Value;

/// Anchor a caller-supplied path/scope under the absolute-path discipline:
/// the server's process cwd is frozen at spawn and cannot track the caller's
/// live directory, so a relative path is only resolvable when an absolute
/// `root` is supplied to anchor it.
///
/// - **Absolute** path → used as-is (`root` ignored).
/// - **Relative** path + **absolute** `root` → joined under `root`.
/// - **Relative** path + **relative** `root` → `Err` (a relative root
///   reintroduces the cwd hazard it was meant to remove).
/// - **Relative** path + **no** `root` → `Err`.
///
/// `label` names the offending input in the error (e.g. `path` / `scope`).
fn anchor_path(
    raw: &std::path::Path,
    root: Option<&std::path::Path>,
    label: &str,
) -> Result<PathBuf, String> {
    if raw.is_absolute() {
        return Ok(raw.to_path_buf());
    }
    match root {
        Some(r) if r.is_absolute() => Ok(r.join(raw)),
        Some(r) => Err(format!(
            "relative {label} \"{}\" cannot be resolved: \"root\" is itself relative (\"{}\"). \
             Set \"root\" to an absolute checkout directory (the server cannot see your shell's cwd).",
            raw.display(),
            r.display(),
        )),
        None => Err(format!(
            "relative {label} \"{}\" cannot be resolved: pass an absolute {label}, or set \"root\" \
             to an absolute checkout directory (the server cannot see your shell's cwd).",
            raw.display(),
        )),
    }
}

/// Resolve the `scope` arg under the absolute-path discipline (`anchor_path`).
///
/// The require-root discipline fires ONLY when a caller EXPLICITLY passes a
/// relative `scope` without an absolute `root` — that is a deliberate but
/// unresolvable request, since the server cannot see the caller's shell cwd.
///
/// When `scope` is **absent entirely**, this falls back to today's default
/// behavior (server launch cwd, exactly as on `main`): no refusal, no `root`
/// requirement. A bare repo-wide search (no `scope`, no `root`) must keep
/// working — that is the default flow of every session.
///
/// When the anchored path does not resolve to an existing directory:
/// - If an absolute `root` is available, fall back to `root` with a soft warning
///   (the caller's checkout exists; the scope subdir simply does not).
/// - If no absolute `root` is available, return `Err` — there is no safe anchor
///   to fall back to, and silently searching the server cwd is the worktree hazard.
///   (This branch is unreachable for an absent `scope`, since `"."` always
///   resolves to an existing directory — the server cwd.)
pub(super) fn resolve_scope(
    args: &Value,
    root: Option<&std::path::Path>,
) -> Result<(PathBuf, Option<String>), String> {
    let scope_arg = args.get("scope").and_then(|v| v.as_str());
    let raw_str = scope_arg.unwrap_or(".");
    let raw: PathBuf = raw_str.into();

    // No explicit scope: behave exactly like main's default-cwd flow. Do not
    // apply the require-root discipline to a value the caller never passed.
    if scope_arg.is_none() {
        let resolved = raw.canonicalize().unwrap_or(raw);
        let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
        if resolved == cwd {
            return Ok((".".into(), None));
        }
        if !resolved.is_dir() {
            return Ok((
                ".".into(),
                Some(format!(
                    "scope \"{raw_str}\" is not a valid directory, searching current directory instead.\n\n"
                )),
            ));
        }
        return Ok((resolved, None));
    }

    let anchored = anchor_path(&raw, root, "scope")?;
    let resolved = anchored.canonicalize().unwrap_or(anchored);
    if !resolved.is_dir() {
        // A missing-dir fallback to "." (server cwd) is the exact worktree hazard
        // this PR closes: the server cwd is frozen at spawn and may point at the
        // wrong checkout. Use root when available (that IS the caller's checkout);
        // error when there is no safe anchor.
        return match root {
            Some(r) if r.is_absolute() => Ok((
                r.to_path_buf(),
                Some(format!(
                    "scope \"{raw_str}\" is not a valid directory, searching the root/checkout directory instead.\n\n"
                )),
            )),
            _ => Err(format!(
                "scope \"{raw_str}\" is not a valid directory and no absolute root was provided to fall back to. \
                 Pass an absolute scope or set \"root\" to an absolute checkout directory."
            )),
        };
    }
    Ok((resolved, None))
}

/// Resolve a relative read path under the absolute-path discipline
/// (`anchor_path`). Absolute paths are used as-is; a relative path requires an
/// absolute `root`, otherwise it is unresolvable (the server cannot see the
/// caller's shell cwd).
pub(super) fn resolve_read_path(
    path: &std::path::Path,
    root: Option<&std::path::Path>,
) -> Result<PathBuf, String> {
    anchor_path(path, root, "path")
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
    fn resolve_scope_explicit_absolute_arg() {
        let tmp = tempfile::tempdir().unwrap();
        let args = serde_json::json!({ "scope": tmp.path().to_str().unwrap() });
        let (scope, warning) = resolve_scope(&args, None).unwrap();
        assert_eq!(scope, tmp.path().canonicalize().unwrap());
        assert!(warning.is_none());
    }

    #[test]
    fn resolve_scope_no_arg_no_root_defaults_to_cwd() {
        // WHY: the require-root discipline fires ONLY when a caller EXPLICITLY
        // passes a relative scope/path without an absolute root. An omitted
        // `scope` must keep today's default behavior (server launch cwd, exactly
        // as on main) — refusing here would break the default flow of every
        // session (e.g. a bare tilth_search/tilth_files call with no scope).
        let args = serde_json::json!({});
        let (scope, warning) = resolve_scope(&args, None).unwrap();
        assert_eq!(scope, PathBuf::from("."));
        assert!(warning.is_none());
    }

    #[test]
    fn resolve_scope_no_arg_ignores_root() {
        // An omitted scope must default to cwd even when `root` IS supplied —
        // `root` only matters when the caller explicitly passes something
        // relative to anchor.
        let args = serde_json::json!({});
        let tmp = tempfile::tempdir().unwrap();
        let (scope, warning) = resolve_scope(&args, Some(tmp.path())).unwrap();
        assert_eq!(scope, PathBuf::from("."));
        assert!(warning.is_none());
    }

    #[test]
    fn resolve_scope_relative_arg_no_root_errors() {
        // A relative scope with no root is unresolvable (same hazard as omitted).
        let args = serde_json::json!({ "scope": "src" });
        let err = resolve_scope(&args, None).unwrap_err();
        assert!(err.contains("relative scope"), "got: {err}");
    }

    #[test]
    fn resolve_scope_relative_arg_relative_root_errors() {
        // A relative root reintroduces the cwd hazard, so it must be refused.
        let args = serde_json::json!({ "scope": "src" });
        let err = resolve_scope(&args, Some(std::path::Path::new("relative/root"))).unwrap_err();
        assert!(
            err.contains("root") && err.contains("relative"),
            "relative root must be refused: {err}"
        );
    }

    #[test]
    fn resolve_scope_missing_dir_with_root_warns_and_returns_root() {
        // WHY: a missing anchored scope must fall back to the caller's root, NOT
        // to "." (server cwd). "." is the server's frozen process-cwd — in a
        // worktree setup this is the wrong checkout. root IS the caller's checkout
        // and is safe to use as the fallback.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let args = serde_json::json!({ "scope": "/nonexistent/directory/zzz" });
        let (scope, warning) = resolve_scope(&args, Some(root)).unwrap();
        assert_eq!(scope, root, "fallback must be root, not server cwd");
        assert!(
            warning.is_some() && warning.unwrap().contains("not a valid directory"),
            "a soft warning must be present naming the issue"
        );
    }

    #[test]
    fn resolve_scope_missing_dir_no_root_errors() {
        // WHY: with no root there is no safe fallback — falling back to server cwd
        // is the worktree wrong-checkout hazard this PR closes. Must be a hard error.
        let args = serde_json::json!({ "scope": "/nonexistent/directory/zzz" });
        let err = resolve_scope(&args, None).unwrap_err();
        assert!(
            err.contains("/nonexistent/directory/zzz") && err.contains("not a valid directory"),
            "error must name the missing scope: {err}"
        );
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

    #[test]
    fn resolve_read_path_relative_anchors_under_absolute_root() {
        // Guards the spec contract: a relative path + absolute root resolves under
        // root, not under the server's cwd. Prevents worktree agents from silently
        // reading the parent checkout.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let result = resolve_read_path(std::path::Path::new("src/foo.rs"), Some(root)).unwrap();
        assert_eq!(result, root.join("src/foo.rs"));
    }

    #[test]
    fn resolve_read_path_absolute_unaffected_by_root() {
        // Absolute paths must be used as-is regardless of root.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let abs = std::path::Path::new("/tmp/other/file.rs");
        let result = resolve_read_path(abs, Some(root)).unwrap();
        assert_eq!(result, abs);
    }

    #[test]
    fn resolve_read_path_relative_no_root_errors() {
        // WHY (inverted from the old "no root → cwd-relative" guard): the old
        // behavior WAS the worktree bug — a relative path silently resolved
        // against the frozen server cwd. It must now refuse with an actionable
        // message naming the path and the absolute-root escape hatch.
        let err = resolve_read_path(std::path::Path::new("src/foo.rs"), None).unwrap_err();
        assert!(
            err.contains("src/foo.rs") && err.contains("root"),
            "refusal must name the path and the root option: {err}"
        );
    }

    #[test]
    fn resolve_read_path_relative_relative_root_errors() {
        // A relative root reintroduces the cwd hazard, so it must be refused too.
        let err = resolve_read_path(
            std::path::Path::new("src/foo.rs"),
            Some(std::path::Path::new("relative/root")),
        )
        .unwrap_err();
        assert!(
            err.contains("root") && err.contains("relative"),
            "relative root must be refused: {err}"
        );
    }

    #[test]
    fn resolve_scope_with_absolute_root_anchors_relative_scope() {
        // resolve_scope(Some(abs_root)) must resolve a relative scope under root,
        // not under cwd — same contract as resolve_read_path.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub = root.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let args = serde_json::json!({ "scope": "sub" });
        let (scope, warning) = resolve_scope(&args, Some(root)).unwrap();
        assert_eq!(scope, sub.canonicalize().unwrap());
        assert!(warning.is_none());
    }

    #[test]
    fn anchor_path_dotdot_not_normalized() {
        // WHY: anchor_path uses root.join(raw) without normalizing `..` components.
        // A path like "../../y" with root "/x" produces "/x/../../y", not "/y".
        // This pins the current behavior so any future traversal normalization is
        // a deliberate, reviewed change — not an accidental side-effect.
        let root = std::path::Path::new("/x");
        let raw = std::path::Path::new("../../y");
        let result = anchor_path(raw, Some(root), "path").unwrap();
        assert_eq!(result, std::path::PathBuf::from("/x/../../y"));
    }
}
