//! MCP tool dispatchers. Each `tilth_*` tool has its own file here; this
//! module exposes their entry points to `super::dispatch_tool`.
//!
//! Tool dispatchers are private to `crate::mcp`: they're invoked by the
//! JSON-RPC handler and never called from outside the server.

use std::path::PathBuf;

use serde_json::Value;

mod definitions;
mod deps;
mod diff;
mod files;
mod list;
mod read;
mod search;
mod session;
mod write;

pub(super) use definitions::tool_definitions;
pub(super) use deps::tool_deps;
pub(super) use diff::tool_diff;
#[cfg(test)]
pub(super) use files::tool_files;
pub(super) use list::tool_list;
pub(super) use read::tool_read;
pub(super) use search::tool_search;
pub(super) use session::tool_session;
#[cfg(test)]
pub(super) use write::tool_edit;
pub(super) use write::tool_write;

/// Resolve the `scope` argument to a canonical directory. Falls back to cwd
/// with a warning message when the argument is missing, invalid, or not a
/// directory.
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

/// Apply an optional token budget to an output string.
pub(super) fn apply_budget(output: String, budget: Option<u64>) -> String {
    match budget {
        Some(b) => crate::budget::apply(&output, b),
        None => output,
    }
}
