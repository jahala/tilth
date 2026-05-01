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
//   opencode:       ~/.config/opencode/opencode.json          (user scope, local entry)
//   gemini:         ~/.gemini/settings.json                   (user scope)
//   codex:          ~/.codex/config.toml                      (user scope, TOML)
//   amp:            ~/.config/amp/settings.json                (user scope)
//   droid:          ~/.factory/mcp.json                        (user scope)
//   antigravity:    ~/.gemini/antigravity/mcp_config.json      (user scope)
//   zed:            ~/.config/zed/settings.json                (user scope)
//   copilot-cli:    ~/.copilot/mcp-config.json                 (user scope)
//   augment:        ~/.augment/settings.json                   (user scope)
//   kiro:           ~/.kiro/settings/mcp.json                  (user scope)
//   kilo-code:      <globalStorage>/kilocode.kilo-code/...     (user scope)
//   cline:          <globalStorage>/saoudrizwan.claude-dev/... (user scope)
//   roo-code:       <globalStorage>/rooveterinaryinc.roo-cline/... (user scope)
//   trae:           .trae/mcp.json                             (project scope)
//   qwen-code:      ~/.qwen/settings.json                     (user scope)
//   crush:          ~/.config/crush/crush.json                 (user scope)
//   pi:             ~/.pi/agent/mcp.json                       (user scope)
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
    "zed",
    "copilot-cli",
    "augment",
    "kiro",
    "kilo-code",
    "cline",
    "roo-code",
    "trae",
    "qwen-code",
    "crush",
    "pi",
];

/// The tilth server entry as JSON. Format depends on the host's ConfigFormat variant.
fn tilth_server_entry(edit: bool, format: &ConfigFormat) -> Value {
    let (command, args) = tilth_command_and_args(edit);
    match format {
        ConfigFormat::Json { .. } => json!({
            "command": command,
            "args": args
        }),
        ConfigFormat::JsonLocal { .. } => {
            let mut command_arr = vec![command];
            command_arr.extend(args);
            json!({
                "type": "local",
                "command": command_arr
            })
        }
        ConfigFormat::Toml => json!({
            "command": command,
            "args": args
        }),
    }
}

/// Write MCP config for the given host, preserving existing config.
pub fn run(host: &str, edit: bool) -> Result<(), String> {
    let host_info = resolve_host(host)?;

    if let Some(parent) = host_info.path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }

    match host_info.format {
        ConfigFormat::Json { .. } | ConfigFormat::JsonLocal { .. } => {
            write_json_config(&host_info, edit)?
        }
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
        ConfigFormat::JsonLocal { servers_key } => servers_key,
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

    upsert_json_server(
        &mut config,
        servers_key,
        tilth_server_entry(edit, &host_info.format),
    )?;

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
    /// JSON with a configurable servers key, using standard {command, args} entry shape.
    Json { servers_key: &'static str },
    /// JSON with a configurable servers key, using opencode's local entry shape {type, command[]}.
    JsonLocal { servers_key: &'static str },
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

        // OpenCode user scope: ~/.config/opencode/opencode.json → mcp (local entry shape)
        "opencode" => Ok(HostInfo {
            path: home.join(".config/opencode/opencode.json"),
            format: ConfigFormat::JsonLocal {
                servers_key: "mcp",
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
        "amp" => Ok(HostInfo {
            path: home.join(".config/amp/settings.json"),
            format: ConfigFormat::Json {
                servers_key: "amp.mcpServers",
            },
            note: Some("User scope — available in all projects."),
        }),

        // Google Antigravity user scope: ~/.gemini/antigravity/mcp_config.json → mcpServers
        "antigravity" => Ok(HostInfo {
            path: home.join(".gemini/antigravity/mcp_config.json"),
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: Some("User scope — available in all projects."),
        }),

        // Factory Droid user scope: ~/.factory/mcp.json → mcpServers
        "droid" => Ok(HostInfo {
            path: home.join(".factory/mcp.json"),
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: Some("User scope — available in all projects."),
        }),

        // Zed user scope: ~/.config/zed/settings.json → context_servers (NOT mcpServers)
        "zed" => Ok(HostInfo {
            path: home.join(".config/zed/settings.json"),
            format: ConfigFormat::Json {
                servers_key: "context_servers",
            },
            note: Some("User scope — available in all projects."),
        }),

        // GitHub Copilot CLI user scope: ~/.copilot/mcp-config.json → mcpServers
        "copilot-cli" => Ok(HostInfo {
            path: home.join(".copilot/mcp-config.json"),
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: Some("User scope — available in all projects."),
        }),

        // AugmentCode user scope: ~/.augment/settings.json → mcpServers
        "augment" => Ok(HostInfo {
            path: home.join(".augment/settings.json"),
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: Some("User scope — available in all projects."),
        }),

        // Kiro user scope: ~/.kiro/settings/mcp.json → mcpServers
        "kiro" => Ok(HostInfo {
            path: home.join(".kiro/settings/mcp.json"),
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: Some("User scope — available in all projects."),
        }),

        // Kilo Code (VS Code extension): globalStorage → mcpServers
        "kilo-code" => Ok(HostInfo {
            path: vscode_global_storage_path("kilocode.kilo-code", "mcp_settings.json")?,
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: None,
        }),

        // Cline (VS Code extension): globalStorage → mcpServers
        "cline" => Ok(HostInfo {
            path: vscode_global_storage_path("saoudrizwan.claude-dev", "cline_mcp_settings.json")?,
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: None,
        }),

        // Roo Code (VS Code extension): globalStorage → mcpServers
        "roo-code" => Ok(HostInfo {
            path: vscode_global_storage_path("rooveterinaryinc.roo-cline", "mcp_settings.json")?,
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: None,
        }),

        // Trae project scope: .trae/mcp.json → mcpServers
        "trae" => Ok(HostInfo {
            path: PathBuf::from(".trae/mcp.json"),
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: Some("Project scope — run from your project root."),
        }),

        // Qwen Code user scope: ~/.qwen/settings.json → mcpServers
        "qwen-code" => Ok(HostInfo {
            path: home.join(".qwen/settings.json"),
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: Some("User scope — available in all projects."),
        }),

        // Crush user scope: ~/.config/crush/crush.json → mcp (NOT mcpServers)
        "crush" => Ok(HostInfo {
            path: home.join(".config/crush/crush.json"),
            format: ConfigFormat::Json {
                servers_key: "mcp",
            },
            note: Some("User scope — available in all projects."),
        }),

        // Pi coding agent user scope: ~/.pi/agent/mcp.json → mcpServers
        "pi" => Ok(HostInfo {
            path: home.join(".pi/agent/mcp.json"),
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

/// Returns the VS Code globalStorage path for a given extension and settings filename.
fn vscode_global_storage_path(extension_id: &str, filename: &str) -> Result<PathBuf, String> {
    let base = vscode_global_storage_base()?;
    Ok(base.join(extension_id).join("settings").join(filename))
}

fn vscode_global_storage_base() -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    {
        let home = home_dir()?;
        Ok(home.join("Library/Application Support/Code/User/globalStorage"))
    }

    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").map_err(|_| "APPDATA not set")?;
        Ok(PathBuf::from(appdata).join("Code/User/globalStorage"))
    }

    #[cfg(target_os = "linux")]
    {
        let home = home_dir()?;
        Ok(home.join(".config/Code/User/globalStorage"))
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err("VS Code globalStorage path unknown on this OS".into())
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
            ConfigFormat::JsonLocal { .. } => panic!("amp should use Json format, not JsonLocal"),
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
            ConfigFormat::JsonLocal { .. } => panic!("droid should use Json format, not JsonLocal"),
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
            ConfigFormat::JsonLocal { .. } => panic!("antigravity should use Json format, not JsonLocal"),
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
    fn zed_resolve_host() {
        let info = resolve_host("zed").expect("zed should resolve");
        assert!(
            info.path.ends_with(".config/zed/settings.json"),
            "path should end with .config/zed/settings.json, got: {}",
            info.path.display()
        );
        match info.format {
            ConfigFormat::Json { servers_key } => {
                assert_eq!(servers_key, "context_servers");
            }
            ConfigFormat::JsonLocal { .. } => panic!("zed should use Json format, not JsonLocal"),
            ConfigFormat::Toml => panic!("zed should use JSON format, not TOML"),
        }
    }

    #[test]
    fn zed_uses_context_servers_not_mcp_servers() {
        let mut config = json!({});
        let entry = json!({"command": "tilth", "args": ["--mcp"]});
        upsert_json_server(&mut config, "context_servers", entry).unwrap();

        assert!(config.get("context_servers").is_some());
        assert!(config.get("mcpServers").is_none());
        assert_eq!(
            config["context_servers"]["tilth"]["command"],
            json!("tilth")
        );
    }

    #[test]
    fn copilot_cli_resolve_host() {
        let info = resolve_host("copilot-cli").expect("copilot-cli should resolve");
        assert!(
            info.path.ends_with(".copilot/mcp-config.json"),
            "path should end with .copilot/mcp-config.json, got: {}",
            info.path.display()
        );
        match info.format {
            ConfigFormat::Json { servers_key } => {
                assert_eq!(servers_key, "mcpServers");
            }
            ConfigFormat::JsonLocal { .. } => panic!("copilot-cli should use Json format, not JsonLocal"),
            ConfigFormat::Toml => panic!("copilot-cli should use JSON format, not TOML"),
        }
    }

    #[test]
    fn augment_resolve_host() {
        let info = resolve_host("augment").expect("augment should resolve");
        assert!(
            info.path.ends_with(".augment/settings.json"),
            "path should end with .augment/settings.json, got: {}",
            info.path.display()
        );
        match info.format {
            ConfigFormat::Json { servers_key } => {
                assert_eq!(servers_key, "mcpServers");
            }
            ConfigFormat::JsonLocal { .. } => panic!("augment should use Json format, not JsonLocal"),
            ConfigFormat::Toml => panic!("augment should use JSON format, not TOML"),
        }
    }

    #[test]
    fn kiro_resolve_host() {
        let info = resolve_host("kiro").expect("kiro should resolve");
        assert!(
            info.path.ends_with(".kiro/settings/mcp.json"),
            "path should end with .kiro/settings/mcp.json, got: {}",
            info.path.display()
        );
        match info.format {
            ConfigFormat::Json { servers_key } => {
                assert_eq!(servers_key, "mcpServers");
            }
            ConfigFormat::JsonLocal { .. } => panic!("kiro should use Json format, not JsonLocal"),
            ConfigFormat::Toml => panic!("kiro should use JSON format, not TOML"),
        }
    }

    #[test]
    fn kilo_code_resolve_host() {
        let info = resolve_host("kilo-code").expect("kilo-code should resolve");
        let path_str = info.path.to_string_lossy();
        assert!(
            path_str.contains("kilocode.kilo-code") && path_str.contains("mcp_settings.json"),
            "path should contain kilocode.kilo-code and mcp_settings.json, got: {path_str}",
        );
        match info.format {
            ConfigFormat::Json { servers_key } => {
                assert_eq!(servers_key, "mcpServers");
            }
            ConfigFormat::JsonLocal { .. } => panic!("kilo-code should use Json format, not JsonLocal"),
            ConfigFormat::Toml => panic!("kilo-code should use JSON format, not TOML"),
        }
    }

    #[test]
    fn cline_resolve_host() {
        let info = resolve_host("cline").expect("cline should resolve");
        let path_str = info.path.to_string_lossy();
        assert!(
            path_str.contains("saoudrizwan.claude-dev")
                && path_str.contains("cline_mcp_settings.json"),
            "path should contain saoudrizwan.claude-dev and cline_mcp_settings.json, got: {path_str}",
        );
        match info.format {
            ConfigFormat::Json { servers_key } => {
                assert_eq!(servers_key, "mcpServers");
            }
            ConfigFormat::JsonLocal { .. } => panic!("cline should use Json format, not JsonLocal"),
            ConfigFormat::Toml => panic!("cline should use JSON format, not TOML"),
        }
    }

    #[test]
    fn roo_code_resolve_host() {
        let info = resolve_host("roo-code").expect("roo-code should resolve");
        let path_str = info.path.to_string_lossy();
        assert!(
            path_str.contains("rooveterinaryinc.roo-cline")
                && path_str.contains("mcp_settings.json"),
            "path should contain rooveterinaryinc.roo-cline and mcp_settings.json, got: {path_str}",
        );
        match info.format {
            ConfigFormat::Json { servers_key } => {
                assert_eq!(servers_key, "mcpServers");
            }
            ConfigFormat::JsonLocal { .. } => panic!("roo-code should use Json format, not JsonLocal"),
            ConfigFormat::Toml => panic!("roo-code should use JSON format, not TOML"),
        }
    }

    #[test]
    fn trae_resolve_host() {
        let info = resolve_host("trae").expect("trae should resolve");
        assert!(
            info.path.ends_with(".trae/mcp.json"),
            "path should end with .trae/mcp.json, got: {}",
            info.path.display()
        );
        match info.format {
            ConfigFormat::Json { servers_key } => {
                assert_eq!(servers_key, "mcpServers");
            }
            ConfigFormat::JsonLocal { .. } => panic!("trae should use Json format, not JsonLocal"),
            ConfigFormat::Toml => panic!("trae should use JSON format, not TOML"),
        }
        assert_eq!(
            info.note,
            Some("Project scope — run from your project root.")
        );
    }

    #[test]
    fn qwen_code_resolve_host() {
        let info = resolve_host("qwen-code").expect("qwen-code should resolve");
        assert!(
            info.path.ends_with(".qwen/settings.json"),
            "path should end with .qwen/settings.json, got: {}",
            info.path.display()
        );
        match info.format {
            ConfigFormat::Json { servers_key } => {
                assert_eq!(servers_key, "mcpServers");
            }
            ConfigFormat::JsonLocal { .. } => panic!("qwen-code should use Json format, not JsonLocal"),
            ConfigFormat::Toml => panic!("qwen-code should use JSON format, not TOML"),
        }
    }

    #[test]
    fn crush_resolve_host() {
        let info = resolve_host("crush").expect("crush should resolve");
        assert!(
            info.path.ends_with(".config/crush/crush.json"),
            "path should end with .config/crush/crush.json, got: {}",
            info.path.display()
        );
        match info.format {
            ConfigFormat::Json { servers_key } => {
                assert_eq!(servers_key, "mcp");
            }
            ConfigFormat::JsonLocal { .. } => panic!("crush should use Json format, not JsonLocal"),
            ConfigFormat::Toml => panic!("crush should use JSON format, not TOML"),
        }
    }

    #[test]
    fn crush_uses_mcp_not_mcp_servers() {
        let mut config = json!({});
        let entry = json!({"command": "tilth", "args": ["--mcp"]});
        upsert_json_server(&mut config, "mcp", entry).unwrap();

        assert!(config.get("mcp").is_some());
        assert!(config.get("mcpServers").is_none());
        assert_eq!(config["mcp"]["tilth"]["command"], json!("tilth"));
    }

    #[test]
    fn pi_resolve_host() {
        let info = resolve_host("pi").expect("pi should resolve");
        assert!(
            info.path.ends_with(".pi/agent/mcp.json"),
            "path should end with .pi/agent/mcp.json, got: {}",
            info.path.display()
        );
        match info.format {
            ConfigFormat::Json { servers_key } => {
                assert_eq!(servers_key, "mcpServers");
            }
            ConfigFormat::JsonLocal { .. } => panic!("pi should use Json format, not JsonLocal"),
            ConfigFormat::Toml => panic!("pi should use JSON format, not TOML"),
        }
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

    #[test]
    fn opencode_resolve_host() {
        let info = resolve_host("opencode").expect("opencode should resolve");
        assert!(
            info.path.ends_with(".config/opencode/opencode.json"),
            "path should end with .config/opencode/opencode.json, got: {}",
            info.path.display()
        );
        match info.format {
            ConfigFormat::JsonLocal { servers_key } => {
                assert_eq!(servers_key, "mcp");
            }
            ConfigFormat::Json { .. } => panic!("opencode should use JsonLocal format, not Json"),
            ConfigFormat::Toml => panic!("opencode should use JSON format, not TOML"),
        }
    }

    #[test]
    fn opencode_entry_uses_local_shape() {
        let entry = tilth_server_entry(false, &ConfigFormat::JsonLocal { servers_key: "mcp" });
        assert_eq!(entry["type"], json!("local"));
        assert!(entry["command"].is_array());
        assert!(entry.get("args").is_none());
    }

    #[test]
    fn opencode_entry_with_edit() {
        let entry = tilth_server_entry(true, &ConfigFormat::JsonLocal { servers_key: "mcp" });
        assert_eq!(entry["type"], json!("local"));
        let cmd = entry["command"].as_array().unwrap();
        assert!(cmd.iter().any(|v| v == "--edit"));
        assert!(cmd.iter().any(|v| v == "--mcp"));
    }

    #[test]
    fn standard_entry_format() {
        let entry = tilth_server_entry(false, &ConfigFormat::Json {
            servers_key: "mcpServers",
        });
        assert!(entry.get("type").is_none());
        assert!(entry["command"].is_string());
        assert!(entry["args"].is_array());
    }

    #[test]
    fn opencode_upserts_under_mcp_key() {
        let mut config = json!({});
        let entry = tilth_server_entry(false, &ConfigFormat::JsonLocal { servers_key: "mcp" });
        upsert_json_server(&mut config, "mcp", entry).unwrap();

        assert!(config.get("mcp").is_some());
        assert!(config.get("mcpServers").is_none());
        assert_eq!(config["mcp"]["tilth"]["type"], json!("local"));
        assert!(config["mcp"]["tilth"]["command"].is_array());
    }
}