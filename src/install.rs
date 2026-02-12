use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};

// Supported MCP hosts and their config locations.
//
// Paths verified from official docs (2025):
//   claude-code:    ~/.claude.json                            (user scope)
//   cursor:         ~/.cursor/mcp.json                        (global)
//   windsurf:       ~/.codeium/windsurf/mcp_config.json       (global)
//   vscode:         .vscode/mcp.json                          (project scope)
//   claude-desktop: ~/Library/Application Support/Claude/...  (global)
const SUPPORTED_HOSTS: &[&str] = &[
    "claude-code",
    "cursor",
    "windsurf",
    "vscode",
    "claude-desktop",
];

/// The tilth server entry injected into each host config.
fn tilth_server_entry(edit: bool) -> Value {
    let args = if edit {
        json!(["--mcp", "--edit"])
    } else {
        json!(["--mcp"])
    };
    json!({
        "command": "tilth",
        "args": args
    })
}

/// Write MCP config for the given host, preserving existing config.
pub fn run(host: &str, edit: bool) -> Result<(), String> {
    let host_info = resolve_host(host)?;

    let mut config: Value = if host_info.path.exists() {
        let raw = fs::read_to_string(&host_info.path)
            .map_err(|e| format!("failed to read {}: {e}", host_info.path.display()))?;
        serde_json::from_str(&raw)
            .map_err(|e| format!("invalid JSON in {}: {e}", host_info.path.display()))?
    } else {
        json!({})
    };

    // VS Code uses "servers" key; all others use "mcpServers"
    let servers_key = host_info.servers_key;

    config
        .as_object_mut()
        .ok_or("config root is not a JSON object")?
        .entry(servers_key)
        .or_insert(json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("{servers_key} is not a JSON object"))?
        .insert("tilth".into(), tilth_server_entry(edit));

    if let Some(parent) = host_info.path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }

    let out =
        serde_json::to_string_pretty(&config).expect("serde_json::Value is always serializable");
    fs::write(&host_info.path, &out)
        .map_err(|e| format!("failed to write {}: {e}", host_info.path.display()))?;

    if edit {
        eprintln!("✓ tilth (edit mode) added to {}", host_info.path.display());
    } else {
        eprintln!("✓ tilth added to {}", host_info.path.display());
    }
    if let Some(note) = host_info.note {
        eprintln!("  {note}");
    }
    Ok(())
}

struct HostInfo {
    path: PathBuf,
    /// JSON key holding the servers map ("mcpServers" or "servers").
    servers_key: &'static str,
    /// Optional note printed after success.
    note: Option<&'static str>,
}

fn resolve_host(host: &str) -> Result<HostInfo, String> {
    let home = home_dir()?;

    match host {
        // Claude Code user scope: ~/.claude.json → mcpServers
        // Available in all projects without checking into source control.
        "claude-code" => Ok(HostInfo {
            path: home.join(".claude.json"),
            servers_key: "mcpServers",
            note: Some("User scope — available in all projects."),
        }),

        // Cursor global: ~/.cursor/mcp.json → mcpServers
        "cursor" => Ok(HostInfo {
            path: home.join(".cursor/mcp.json"),
            servers_key: "mcpServers",
            note: None,
        }),

        // Windsurf global: ~/.codeium/windsurf/mcp_config.json → mcpServers
        "windsurf" => Ok(HostInfo {
            path: home.join(".codeium/windsurf/mcp_config.json"),
            servers_key: "mcpServers",
            note: None,
        }),

        // VS Code project scope: .vscode/mcp.json → servers (NOT mcpServers)
        "vscode" => Ok(HostInfo {
            path: PathBuf::from(".vscode/mcp.json"),
            servers_key: "servers",
            note: Some("Project scope — run from your project root."),
        }),

        "claude-desktop" => Ok(HostInfo {
            path: claude_desktop_path()?,
            servers_key: "mcpServers",
            note: None,
        }),

        _ => Err(format!(
            "unknown host: {host}. Supported: {}",
            SUPPORTED_HOSTS.join(", ")
        )),
    }
}

fn home_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    {
        std::env::var("USERPROFILE")
            .map(PathBuf::from)
            .map_err(|_| "USERPROFILE not set".into())
    }

    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("HOME")
            .map(PathBuf::from)
            .map_err(|_| "HOME not set".into())
    }
}

fn claude_desktop_path() -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    {
        let home = home_dir()?;
        Ok(home.join("Library/Application Support/Claude/claude_desktop_config.json"))
    }

    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").map_err(|_| "APPDATA not set")?;
        Ok(PathBuf::from(appdata).join("Claude/claude_desktop_config.json"))
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err("claude-desktop config path unknown on this OS".into())
    }
}
