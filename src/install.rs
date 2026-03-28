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
//   opencode:       ~/.opencode.json                          (user scope)
//   gemini:         ~/.gemini/settings.json                   (user scope)
//   codex:          ~/.codex/config.toml                      (user scope, TOML)
//   amp:            ~/.config/amp/settings.json                (user scope)
//   droid:          ~/.factory/mcp.json                        (user scope)
//   antigravity:    ~/.gemini/antigravity/mcp_config.json      (user scope)
const SUPPORTED_HOSTS: &[&str] = &[
    "claude-code",
    "cursor",
    "windsurf",
    "vscode",
    "claude-desktop",
    "opencode",
    "gemini",
    "codex",
    "amp",
    "droid",
    "antigravity",
];

/// The tilth server entry as JSON, for hosts that use JSON config.
fn tilth_server_entry(edit: bool) -> Value {
    let (command, args) = tilth_command_and_args(edit);
    json!({
        "command": command,
        "args": args
    })
}

/// Write MCP config for the given host, preserving existing config.
pub fn run(host: &str, edit: bool) -> Result<(), String> {
    let host_info = resolve_host(host)?;

    if let Some(parent) = host_info.path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }

    match host_info.format {
        ConfigFormat::Json { .. } => write_json_config(&host_info, edit)?,
        ConfigFormat::Toml => write_toml_config(&host_info, edit)?,
    }

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

fn write_json_config(host_info: &HostInfo, edit: bool) -> Result<(), String> {
    let servers_key = match host_info.format {
        ConfigFormat::Json { servers_key } => servers_key,
        ConfigFormat::Toml => unreachable!("write_json_config called for TOML host"),
    };

    let mut config: Value = if host_info.path.exists() {
        let raw = fs::read_to_string(&host_info.path)
            .map_err(|e| format!("failed to read {}: {e}", host_info.path.display()))?;
        serde_json::from_str(&raw)
            .map_err(|e| format!("invalid JSON in {}: {e}", host_info.path.display()))?
    } else {
        json!({})
    };

    upsert_json_server(&mut config, servers_key, tilth_server_entry(edit))?;

    let out =
        serde_json::to_string_pretty(&config).expect("serde_json::Value is always serializable");
    fs::write(&host_info.path, &out)
        .map_err(|e| format!("failed to write {}: {e}", host_info.path.display()))?;
    Ok(())
}

/// Writes a `[mcp_servers.tilth]` section into a TOML config file.
fn write_toml_config(host_info: &HostInfo, edit: bool) -> Result<(), String> {
    let (command, args) = tilth_command_and_args(edit);

    // Escape backslashes for TOML basic strings (Windows paths like C:\Users\...).
    let command_escaped = command.replace('\\', "\\\\");
    let args_toml: Vec<String> = args
        .iter()
        .map(|a| format!("\"{}\"", a.replace('\\', "\\\\")))
        .collect();
    let section = format!(
        "[mcp_servers.tilth]\ncommand = \"{command_escaped}\"\nargs = [{}]\n",
        args_toml.join(", ")
    );

    let existing = if host_info.path.exists() {
        fs::read_to_string(&host_info.path)
            .map_err(|e| format!("failed to read {}: {e}", host_info.path.display()))?
    } else {
        String::new()
    };

    // Remove existing [mcp_servers.tilth] section if present
    let output = if let Some(start) = existing.find("[mcp_servers.tilth]") {
        // Find end of section: next [header] or EOF
        let rest = &existing[start..];
        let end = rest[1..] // skip the opening '['
            .find("\n[")
            .map_or(existing.len(), |i| start + 1 + i + 1);
        format!("{}{}{}", &existing[..start], section, &existing[end..])
    } else {
        // Append with a blank line separator
        let sep = if existing.is_empty() || existing.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        format!("{existing}{sep}\n{section}")
    };

    fs::write(&host_info.path, &output)
        .map_err(|e| format!("failed to write {}: {e}", host_info.path.display()))?;
    Ok(())
}

/// Returns (command, args) for the tilth MCP server entry.
fn tilth_command_and_args(edit: bool) -> (String, Vec<String>) {
    let mut mcp_args: Vec<String> = vec!["--mcp".into()];
    if edit {
        mcp_args.push("--edit".into());
    }

    let via_npm = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.contains("node_modules")))
        .unwrap_or(false);

    if via_npm {
        let mut args = vec!["tilth".to_string()];
        args.extend(mcp_args);
        ("npx".into(), args)
    } else {
        let command = std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(String::from))
            .unwrap_or_else(|| "tilth".into());
        (command, mcp_args)
    }
}

#[derive(Debug)]
enum ConfigFormat {
    /// JSON with a configurable servers key ("mcpServers" or "servers").
    Json { servers_key: &'static str },
    /// TOML with `[mcp_servers.<name>]` sections.
    Toml,
}

struct HostInfo {
    path: PathBuf,
    format: ConfigFormat,
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
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: Some("User scope — available in all projects."),
        }),

        // Cursor global: ~/.cursor/mcp.json → mcpServers
        "cursor" => Ok(HostInfo {
            path: home.join(".cursor/mcp.json"),
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: None,
        }),

        // Windsurf global: ~/.codeium/windsurf/mcp_config.json → mcpServers
        "windsurf" => Ok(HostInfo {
            path: home.join(".codeium/windsurf/mcp_config.json"),
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: None,
        }),

        // VS Code project scope: .vscode/mcp.json → servers (NOT mcpServers)
        "vscode" => Ok(HostInfo {
            path: PathBuf::from(".vscode/mcp.json"),
            format: ConfigFormat::Json {
                servers_key: "servers",
            },
            note: Some("Project scope — run from your project root."),
        }),

        "claude-desktop" => Ok(HostInfo {
            path: claude_desktop_path()?,
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: None,
        }),

        // OpenCode user scope: ~/.opencode.json → mcpServers
        // Verified from opencode source: internal/config/config.go (viper config name ".opencode")
        "opencode" => Ok(HostInfo {
            path: home.join(".opencode.json"),
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: Some("User scope — available in all projects."),
        }),

        // Gemini CLI user scope: ~/.gemini/settings.json → mcpServers
        "gemini" => Ok(HostInfo {
            path: home.join(".gemini/settings.json"),
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: Some("User scope — available in all projects."),
        }),

        // Codex CLI user scope: ~/.codex/config.toml → [mcp_servers.tilth] (TOML)
        "codex" => Ok(HostInfo {
            path: home.join(".codex/config.toml"),
            format: ConfigFormat::Toml,
            note: Some("User scope — available in all projects."),
        }),

        // Amp user scope: ~/.config/amp/settings.json → amp.mcpServers
        // Verified from official docs: https://ampcode.com/manual
        "amp" => Ok(HostInfo {
            path: home.join(".config/amp/settings.json"),
            format: ConfigFormat::Json {
                servers_key: "amp.mcpServers",
            },
            note: Some("User scope — available in all projects."),
        }),

        // Google Antigravity user scope: ~/.gemini/antigravity/mcp_config.json → mcpServers
        // Verified from official docs: https://antigravity.google/docs/mcp
        "antigravity" => Ok(HostInfo {
            path: home.join(".gemini/antigravity/mcp_config.json"),
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: Some("User scope — available in all projects."),
        }),

        // Factory Droid user scope: ~/.factory/mcp.json → mcpServers
        // Verified from official docs: https://docs.factory.ai/cli/configuration/mcp
        "droid" => Ok(HostInfo {
            path: home.join(".factory/mcp.json"),
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: Some("User scope — available in all projects."),
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

/// Merge a tilth server entry into a JSON config under the given servers key.
/// Extracted for testability — used by `write_json_config` and unit tests.
fn upsert_json_server(config: &mut Value, servers_key: &str, entry: Value) -> Result<(), String> {
    config
        .as_object_mut()
        .ok_or("config root is not a JSON object")?
        .entry(servers_key)
        .or_insert(json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("{servers_key} is not a JSON object"))?
        .insert("tilth".into(), entry);
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amp_resolve_host() {
        let info = resolve_host("amp").expect("amp should resolve");
        assert!(
            info.path.ends_with(".config/amp/settings.json"),
            "path should end with .config/amp/settings.json, got: {}",
            info.path.display()
        );
        match info.format {
            ConfigFormat::Json { servers_key } => {
                assert_eq!(servers_key, "amp.mcpServers");
            }
            ConfigFormat::Toml => panic!("amp should use JSON format, not TOML"),
        }
    }

    #[test]
    fn amp_dotted_key_is_literal_not_nested() {
        let mut config = json!({});
        let entry = json!({"command": "tilth", "args": ["--mcp"]});
        upsert_json_server(&mut config, "amp.mcpServers", entry).unwrap();

        // Top-level key must be the literal "amp.mcpServers"
        assert!(
            config.get("amp.mcpServers").is_some(),
            "should have literal top-level key 'amp.mcpServers'"
        );
        // Must NOT create a nested "amp" object
        assert!(
            config.get("amp").is_none(),
            "should NOT have a nested 'amp' key"
        );
        // Verify tilth entry is inside
        assert_eq!(config["amp.mcpServers"]["tilth"]["command"], json!("tilth"));
    }

    #[test]
    fn amp_preserves_unrelated_config() {
        let mut config = json!({
            "amp.theme": "dark",
            "amp.mcpServers": {
                "other": {"command": "foo", "args": []}
            }
        });
        let entry = json!({"command": "tilth", "args": ["--mcp"]});
        upsert_json_server(&mut config, "amp.mcpServers", entry).unwrap();

        assert_eq!(config["amp.theme"], json!("dark"));
        assert_eq!(config["amp.mcpServers"]["other"]["command"], json!("foo"));
        assert!(config["amp.mcpServers"]["tilth"].is_object());
    }

    #[test]
    fn amp_overwrites_existing_tilth() {
        let mut config = json!({
            "amp.mcpServers": {
                "tilth": {"command": "old", "args": ["--old"]}
            }
        });
        let entry = json!({"command": "tilth", "args": ["--mcp"]});
        upsert_json_server(&mut config, "amp.mcpServers", entry).unwrap();

        assert_eq!(config["amp.mcpServers"]["tilth"]["args"], json!(["--mcp"]));
    }

    #[test]
    fn amp_error_when_servers_key_not_object() {
        let mut config = json!({"amp.mcpServers": []});
        let entry = json!({"command": "tilth", "args": ["--mcp"]});
        let err = upsert_json_server(&mut config, "amp.mcpServers", entry).unwrap_err();
        assert!(
            err.contains("amp.mcpServers is not a JSON object"),
            "error should mention the key, got: {err}"
        );
    }

    #[test]
    fn droid_resolve_host() {
        let info = resolve_host("droid").expect("droid should resolve");
        assert!(
            info.path.ends_with(".factory/mcp.json"),
            "path should end with .factory/mcp.json, got: {}",
            info.path.display()
        );
        match info.format {
            ConfigFormat::Json { servers_key } => {
                assert_eq!(servers_key, "mcpServers");
            }
            ConfigFormat::Toml => panic!("droid should use JSON format, not TOML"),
        }
    }

    #[test]
    fn droid_preserves_existing_servers() {
        let mut config = json!({
            "mcpServers": {
                "playwright": {"command": "npx", "args": ["-y", "@playwright/mcp@latest"]}
            }
        });
        let entry = json!({"command": "tilth", "args": ["--mcp"]});
        upsert_json_server(&mut config, "mcpServers", entry).unwrap();

        assert_eq!(config["mcpServers"]["playwright"]["command"], json!("npx"));
        assert!(config["mcpServers"]["tilth"].is_object());
    }

    #[test]
    fn unknown_host_error_includes_droid() {
        let err = resolve_host("nope")
            .err()
            .expect("unknown host should return an error");
        assert!(
            err.contains("droid"),
            "error should list droid in supported hosts, got: {err}"
        );
    }

    #[test]
    fn antigravity_resolve_host() {
        let info = resolve_host("antigravity").expect("antigravity should resolve");
        assert!(
            info.path.ends_with(".gemini/antigravity/mcp_config.json"),
            "path should end with .gemini/antigravity/mcp_config.json, got: {}",
            info.path.display()
        );
        match info.format {
            ConfigFormat::Json { servers_key } => {
                assert_eq!(servers_key, "mcpServers");
            }
            ConfigFormat::Toml => panic!("antigravity should use JSON format, not TOML"),
        }
    }

    #[test]
    fn antigravity_preserves_existing_servers() {
        let mut config = json!({
            "mcpServers": {
                "firebase": {"command": "npx", "args": ["-y", "firebase-tools@latest", "mcp"]}
            }
        });
        let entry = json!({"command": "tilth", "args": ["--mcp"]});
        upsert_json_server(&mut config, "mcpServers", entry).unwrap();

        assert_eq!(config["mcpServers"]["firebase"]["command"], json!("npx"));
        assert!(config["mcpServers"]["tilth"].is_object());
    }

    #[test]
    fn unknown_host_error_includes_antigravity() {
        let err = resolve_host("nope")
            .err()
            .expect("unknown host should return an error");
        assert!(
            err.contains("antigravity"),
            "error should list antigravity in supported hosts, got: {err}"
        );
    }

    #[test]
    fn unknown_host_error_includes_amp() {
        let err = resolve_host("nope")
            .err()
            .expect("unknown host should return an error");
        assert!(
            err.contains("amp"),
            "error should list amp in supported hosts, got: {err}"
        );
    }
}
