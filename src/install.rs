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

/// The tilth cwd-injection `PreToolUse` hook script, embedded from the
/// standalone plugin so `tilth install claude-code` can write it directly
/// instead of requiring a manual `plugin/claude/` install step.
const INJECT_CWD_JS: &str = include_str!("../plugin/claude/hooks/inject-cwd.js");

/// Matcher for the tilth `PreToolUse` hook entry — mirrors `plugin/claude/hooks/hooks.json`.
const TILTH_HOOK_MATCHER: &str = "mcp__tilth__.*";

/// The tilth server entry as JSON. Format depends on the host's [`ConfigFormat`] variant.
fn tilth_server_entry(edit: bool, format: &ConfigFormat, hook_injected: &str) -> Value {
    let (command, args) = tilth_command_and_args(edit);
    let env = json!({ "TILTH_MCP_CWD_HOOK_INJECTED": hook_injected });
    match format {
        ConfigFormat::Json { .. } => json!({
            "command": command,
            "args": args,
            "env": env
        }),
        ConfigFormat::JsonLocal { .. } => {
            let mut command_arr = vec![command];
            command_arr.extend(args);
            json!({
                "type": "local",
                "command": command_arr,
                "environment": env
            })
        }
        ConfigFormat::Toml => unreachable!("tilth_server_entry called for TOML host"),
    }
}

/// Write MCP config for the given host, preserving existing config.
///
/// For claude-code (unless `no_hook`), also writes the cwd-injection hook
/// script to `~/.claude/tilth/inject-cwd.js` and upserts a `PreToolUse` entry
/// into `~/.claude/settings.json` — a different file from `~/.claude.json`
/// (the MCP server config written above).
pub fn run(host: &str, edit: bool, no_hook: bool) -> Result<(), String> {
    let host_info = resolve_host(host)?;
    // Claude Code ships the cwd-injection hook (auto-installed below unless
    // --no-hook), so its schema tells the model NOT to set cwd; every other
    // host sets it explicitly.
    let hook_injected = if host == "claude-code" { "1" } else { "0" };

    if let Some(parent) = host_info.path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }

    match host_info.format {
        ConfigFormat::Json { .. } | ConfigFormat::JsonLocal { .. } => {
            write_json_config(&host_info, edit, hook_injected)?;
        }
        ConfigFormat::Toml => write_toml_config(&host_info, edit, hook_injected)?,
    }

    if edit {
        eprintln!("✓ tilth (edit mode) added to {}", host_info.path.display());
    } else {
        eprintln!("✓ tilth added to {}", host_info.path.display());
    }

    if host == "claude-code" {
        if no_hook {
            eprintln!(
                "  Hook not installed (--no-hook). Install manually from plugin/claude/, or via the Claude Code plugin marketplace."
            );
        } else {
            let home = home_dir()?;
            let (script_path, settings_path) = install_claude_code_hook(&home)?;
            eprintln!("✓ cwd-injection hook installed");
            eprintln!("  script:   {}", script_path.display());
            eprintln!("  settings: {}", settings_path.display());
        }
    } else if let Some(note) = host_info.note {
        eprintln!("  {note}");
    }

    Ok(())
}

/// Write `content` to `path` atomically: write to a sibling temp file first,
/// then rename over the target so an interrupted write never truncates `path`.
fn atomic_write(path: &std::path::Path, content: &str) -> Result<(), String> {
    crate::util::atomic_write_bytes(path, content.as_bytes())
        .map_err(|e| format!("failed to write {}: {e}", path.display()))
}

fn write_json_config(host_info: &HostInfo, edit: bool, hook_injected: &str) -> Result<(), String> {
    let servers_key = match host_info.format {
        ConfigFormat::Json { servers_key } | ConfigFormat::JsonLocal { servers_key } => servers_key,
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
        tilth_server_entry(edit, &host_info.format, hook_injected),
    )?;

    let out =
        serde_json::to_string_pretty(&config).expect("serde_json::Value is always serializable");
    atomic_write(&host_info.path, &out)?;
    Ok(())
}

/// Builds the `[mcp_servers.tilth]` TOML table for the given command/args/env.
/// Split from `upsert_toml_tilth_table` so a test can feed it arbitrary
/// strings (e.g. containing `"`) without going through `tilth_command_and_args`.
fn build_tilth_toml_table(command: &str, args: &[String], hook_injected: &str) -> toml_edit::Table {
    let mut table = toml_edit::Table::new();
    table["command"] = toml_edit::value(command);

    let mut args_arr = toml_edit::Array::new();
    for a in args {
        args_arr.push(a.as_str());
    }
    table["args"] = toml_edit::value(args_arr);

    let mut env = toml_edit::InlineTable::new();
    env.insert("TILTH_MCP_CWD_HOOK_INJECTED", hook_injected.into());
    table["env"] = toml_edit::value(env);

    table
}

/// Inserts `table` as `[mcp_servers.tilth]` under `root`, creating an
/// implicit `mcp_servers` parent table if absent so it renders as a bare
/// `[mcp_servers.tilth]` header rather than an empty `[mcp_servers]` one.
/// Plain `doc["mcp_servers"]["tilth"] = ...` auto-vivifies as a dotted inline
/// table instead of a real header, which does not round-trip through the
/// `toml` crate parser used elsewhere in this crate.
fn insert_tilth_table(root: &mut toml_edit::Table, table: toml_edit::Table) -> Result<(), String> {
    if !root.contains_key("mcp_servers") {
        let mut parent = toml_edit::Table::new();
        parent.set_implicit(true);
        root.insert("mcp_servers", toml_edit::Item::Table(parent));
    }
    let mcp_servers = root["mcp_servers"]
        .as_table_mut()
        .ok_or("mcp_servers is not a TOML table")?;
    mcp_servers.insert("tilth", toml_edit::Item::Table(table));
    Ok(())
}

/// Inserts/replaces the `[mcp_servers.tilth]` table in a parsed TOML document,
/// preserving every other table, key, and comment via `toml_edit`'s
/// format-preserving edit model.
fn upsert_toml_tilth_table(
    doc: &mut toml_edit::DocumentMut,
    edit: bool,
    hook_injected: &str,
) -> Result<(), String> {
    let (command, args) = tilth_command_and_args(edit);
    let table = build_tilth_toml_table(&command, &args, hook_injected);
    insert_tilth_table(doc.as_table_mut(), table)
}

/// Writes a `[mcp_servers.tilth]` section into a TOML config file, preserving
/// the rest of the document (formatting, comments, other tables) untouched.
fn write_toml_config(host_info: &HostInfo, edit: bool, hook_injected: &str) -> Result<(), String> {
    let existing = if host_info.path.exists() {
        fs::read_to_string(&host_info.path)
            .map_err(|e| format!("failed to read {}: {e}", host_info.path.display()))?
    } else {
        String::new()
    };

    let mut doc: toml_edit::DocumentMut = existing
        .parse()
        .map_err(|e| format!("invalid TOML in {}: {e}", host_info.path.display()))?;

    upsert_toml_tilth_table(&mut doc, edit, hook_injected)?;

    atomic_write(&host_info.path, &doc.to_string())?;
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
            note: None, // hook install / --no-hook messaging is handled inline in `run`
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
            format: ConfigFormat::JsonLocal { servers_key: "mcp" },
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

        // Zed user scope: ~/.config/zed/settings.json → context_servers (NOT mcpServers)
        // Verified from official docs: https://zed.dev/docs/ai/mcp
        "zed" => Ok(HostInfo {
            path: home.join(".config/zed/settings.json"),
            format: ConfigFormat::Json {
                servers_key: "context_servers",
            },
            note: Some("User scope — available in all projects."),
        }),

        // GitHub Copilot CLI user scope: ~/.copilot/mcp-config.json → mcpServers
        // Verified from official docs: https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers
        "copilot-cli" => Ok(HostInfo {
            path: home.join(".copilot/mcp-config.json"),
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: Some("User scope — available in all projects."),
        }),

        // AugmentCode user scope: ~/.augment/settings.json → mcpServers
        // Verified from official docs: https://docs.augmentcode.com/cli/integrations
        "augment" => Ok(HostInfo {
            path: home.join(".augment/settings.json"),
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: Some("User scope — available in all projects."),
        }),

        // Kiro user scope: ~/.kiro/settings/mcp.json → mcpServers
        // Verified from official docs: https://kiro.dev/docs/mcp/configuration/
        "kiro" => Ok(HostInfo {
            path: home.join(".kiro/settings/mcp.json"),
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: Some("User scope — available in all projects."),
        }),

        // Kilo Code (VS Code extension): globalStorage → mcpServers
        // Verified from official docs: https://kilo.ai/docs/automate/mcp/using-in-kilo-code
        "kilo-code" => Ok(HostInfo {
            path: vscode_global_storage_path("kilocode.kilo-code", "mcp_settings.json")?,
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: None,
        }),

        // Cline (VS Code extension): globalStorage → mcpServers
        // Verified from official docs: https://docs.cline.bot/mcp-servers/configuring-mcp-servers
        "cline" => Ok(HostInfo {
            path: vscode_global_storage_path("saoudrizwan.claude-dev", "cline_mcp_settings.json")?,
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: None,
        }),

        // Roo Code (VS Code extension): globalStorage → mcpServers
        // Verified from official docs: https://docs.roocode.com/features/mcp/using-mcp-in-roo
        "roo-code" => Ok(HostInfo {
            path: vscode_global_storage_path("rooveterinaryinc.roo-cline", "mcp_settings.json")?,
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: None,
        }),

        // Trae project scope: .trae/mcp.json → mcpServers
        // Verified from official docs: https://docs.trae.ai/ide/add-mcp-servers
        "trae" => Ok(HostInfo {
            path: PathBuf::from(".trae/mcp.json"),
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: Some("Project scope — run from your project root."),
        }),

        // Qwen Code user scope: ~/.qwen/settings.json → mcpServers
        // Verified from official docs: https://qwenlm.github.io/qwen-code-docs/en/users/features/mcp/
        "qwen-code" => Ok(HostInfo {
            path: home.join(".qwen/settings.json"),
            format: ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            note: Some("User scope — available in all projects."),
        }),

        // Crush user scope: ~/.config/crush/crush.json → mcp (NOT mcpServers)
        // Verified from official docs: https://github.com/charmbracelet/crush
        "crush" => Ok(HostInfo {
            path: home.join(".config/crush/crush.json"),
            format: ConfigFormat::Json { servers_key: "mcp" },
            note: Some("User scope — available in all projects."),
        }),

        // Pi coding agent user scope: ~/.pi/agent/mcp.json → mcpServers
        // Verified from: https://github.com/badlogic/pi-mono/issues/563
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

/// Cross-platform home-directory lookup, with an actionable error message.
fn home_dir() -> Result<PathBuf, String> {
    home::home_dir().ok_or_else(|| "home directory not found ($HOME / $USERPROFILE)".into())
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

/// Idempotently upsert the tilth cwd-injection `PreToolUse` hook entry into a
/// claude-code `settings.json` [`Value`]. Replaces any existing entry whose
/// matcher equals [`TILTH_HOOK_MATCHER`]; appends when none exists. Preserves
/// every other `PreToolUse` entry, every other hook event, and every
/// unrelated top-level settings key. Extracted for testability — used by
/// `install_claude_code_hook` and unit tests.
fn upsert_pretooluse_hook(settings: &mut Value, script_path: &str) -> Result<(), String> {
    let entry = json!({
        "matcher": TILTH_HOOK_MATCHER,
        "hooks": [
            { "type": "command", "command": format!("node \"{script_path}\"") }
        ]
    });

    let root = settings
        .as_object_mut()
        .ok_or("settings root is not a JSON object")?;
    let pre_tool_use = root
        .entry("hooks")
        .or_insert(json!({}))
        .as_object_mut()
        .ok_or("hooks is not a JSON object")?
        .entry("PreToolUse")
        .or_insert(json!([]));
    let entries = pre_tool_use
        .as_array_mut()
        .ok_or("hooks.PreToolUse is not a JSON array")?;

    match entries
        .iter_mut()
        .find(|e| e.get("matcher").and_then(Value::as_str) == Some(TILTH_HOOK_MATCHER))
    {
        Some(existing) => *existing = entry,
        None => entries.push(entry),
    }
    Ok(())
}

/// Writes the cwd-injection hook script to `~/.claude/tilth/inject-cwd.js`
/// and upserts its `PreToolUse` entry into `~/.claude/settings.json` — a
/// different file from `~/.claude.json` (the MCP server config). Returns the
/// (script path, settings path) written, for the success message in `run`.
fn install_claude_code_hook(home: &std::path::Path) -> Result<(PathBuf, PathBuf), String> {
    let script_dir = home.join(".claude/tilth");
    fs::create_dir_all(&script_dir)
        .map_err(|e| format!("failed to create {}: {e}", script_dir.display()))?;
    let script_path = script_dir.join("inject-cwd.js");
    atomic_write(&script_path, INJECT_CWD_JS)?;

    let settings_path = home.join(".claude/settings.json");
    let mut settings: Value = if settings_path.exists() {
        let raw = fs::read_to_string(&settings_path)
            .map_err(|e| format!("failed to read {}: {e}", settings_path.display()))?;
        serde_json::from_str(&raw)
            .map_err(|e| format!("invalid JSON in {}: {e}", settings_path.display()))?
    } else {
        json!({})
    };

    let script_str = script_path.to_str().ok_or_else(|| {
        format!(
            "hook script path is not valid UTF-8: {}",
            script_path.display()
        )
    })?;
    upsert_pretooluse_hook(&mut settings, script_str)?;

    let out =
        serde_json::to_string_pretty(&settings).expect("serde_json::Value is always serializable");
    atomic_write(&settings_path, &out)?;

    Ok((script_path, settings_path))
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
    fn toml_section_appended_when_absent() {
        let mut doc: toml_edit::DocumentMut = "[other]\nk = 1\n".parse().unwrap();
        upsert_toml_tilth_table(&mut doc, false, "0").unwrap();
        let out = doc.to_string();
        assert!(out.contains("[other]"));
        assert!(out.contains("[mcp_servers.tilth]"));
        assert_eq!(doc["other"]["k"].as_integer(), Some(1));
    }

    #[test]
    fn toml_preserves_comments_and_unrelated_section() {
        let existing = "# legacy note about [mcp_servers.tilth] kept for humans\n[other]\nk = 1\n";
        let mut doc: toml_edit::DocumentMut = existing.parse().unwrap();
        upsert_toml_tilth_table(&mut doc, false, "0").unwrap();
        let out = doc.to_string();
        assert!(
            out.contains("# legacy note about [mcp_servers.tilth] kept for humans"),
            "comment lost during upsert: {out:?}"
        );
        assert!(out.contains("[other]"));
        assert_eq!(
            doc["other"]["k"].as_integer(),
            Some(1),
            "unrelated section survived"
        );
        assert!(doc["mcp_servers"]["tilth"]["command"].as_str().is_some());
    }

    #[test]
    fn toml_section_replaces_existing_tilth_table() {
        let existing = "[mcp_servers.tilth]\ncommand = \"old\"\nargs = []\n[other]\nk = 1\n";
        let mut doc: toml_edit::DocumentMut = existing.parse().unwrap();
        upsert_toml_tilth_table(&mut doc, true, "1").unwrap();
        let out = doc.to_string();
        assert!(!out.contains("\"old\""), "old command not removed: {out:?}");
        assert!(out.contains("[other]"));
        assert_eq!(doc["other"]["k"].as_integer(), Some(1));
        assert_eq!(
            doc["mcp_servers"]["tilth"]["env"]["TILTH_MCP_CWD_HOOK_INJECTED"].as_str(),
            Some("1")
        );
    }

    #[test]
    fn toml_quoted_command_and_arg_round_trip() {
        // A `"` in the resolved command path or an arg must not corrupt the
        // emitted TOML — toml_edit escapes it, and re-parsing must recover
        // the exact original string.
        let command = "C:\\Program Files\\has \"quotes\"\\tilth.exe";
        let args = vec!["--mcp".to_string(), "say \"hi\"".to_string()];
        let table = build_tilth_toml_table(command, &args, "0");

        let mut doc = toml_edit::DocumentMut::new();
        insert_tilth_table(doc.as_table_mut(), table).unwrap();
        let text = doc.to_string();

        let reparsed: toml_edit::DocumentMut = text.parse().expect("emitted TOML must parse");
        assert_eq!(
            reparsed["mcp_servers"]["tilth"]["command"].as_str(),
            Some(command)
        );
        let reparsed_args: Vec<&str> = reparsed["mcp_servers"]["tilth"]["args"]
            .as_array()
            .expect("args must be an array")
            .iter()
            .map(|v| v.as_str().expect("arg must be a string"))
            .collect();
        assert_eq!(reparsed_args, vec!["--mcp", "say \"hi\""]);

        // Also verify via the standalone `toml` crate (used elsewhere in this
        // crate) so the fix isn't just self-consistent with toml_edit.
        let via_toml: toml::Value = toml::from_str(&text).expect("toml crate must also parse it");
        assert_eq!(
            via_toml["mcp_servers"]["tilth"]["command"].as_str(),
            Some(command)
        );
    }

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
            ConfigFormat::JsonLocal { .. } => {
                panic!("antigravity should use Json format, not JsonLocal")
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
            ConfigFormat::JsonLocal { .. } => {
                panic!("copilot-cli should use Json format, not JsonLocal")
            }
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
            ConfigFormat::JsonLocal { .. } => {
                panic!("augment should use Json format, not JsonLocal")
            }
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
            ConfigFormat::JsonLocal { .. } => {
                panic!("kilo-code should use Json format, not JsonLocal")
            }
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
            ConfigFormat::JsonLocal { .. } => {
                panic!("roo-code should use Json format, not JsonLocal")
            }
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
            ConfigFormat::JsonLocal { .. } => {
                panic!("qwen-code should use Json format, not JsonLocal")
            }
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
        let entry = tilth_server_entry(false, &ConfigFormat::JsonLocal { servers_key: "mcp" }, "0");
        assert_eq!(entry["type"], json!("local"));
        assert!(entry["command"].is_array());
        assert!(entry.get("args").is_none());
    }

    #[test]
    fn opencode_entry_with_edit() {
        let entry = tilth_server_entry(true, &ConfigFormat::JsonLocal { servers_key: "mcp" }, "0");
        assert_eq!(entry["type"], json!("local"));
        let cmd = entry["command"].as_array().unwrap();
        assert!(cmd.iter().any(|v| v == "--edit"));
        assert!(cmd.iter().any(|v| v == "--mcp"));
    }

    #[test]
    fn standard_entry_format() {
        let entry = tilth_server_entry(
            false,
            &ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            "0",
        );
        assert!(entry.get("type").is_none());
        assert!(entry["command"].is_string());
        assert!(entry["args"].is_array());
    }

    #[test]
    fn opencode_upserts_under_mcp_key() {
        let mut config = json!({});
        let entry = tilth_server_entry(false, &ConfigFormat::JsonLocal { servers_key: "mcp" }, "0");
        upsert_json_server(&mut config, "mcp", entry).unwrap();

        assert!(config.get("mcp").is_some());
        assert!(config.get("mcpServers").is_none());
        assert_eq!(config["mcp"]["tilth"]["type"], json!("local"));
        assert!(config["mcp"]["tilth"]["command"].is_array());
    }

    /// `tilth install` writes the cwd-hook env var into the server entry: "1"
    /// for claude-code (the hook injects cwd), "0" for every other host.
    #[test]
    fn server_entry_carries_hook_injected_env() {
        let claude = tilth_server_entry(
            false,
            &ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            "1",
        );
        assert_eq!(claude["env"]["TILTH_MCP_CWD_HOOK_INJECTED"], json!("1"));

        let other = tilth_server_entry(
            false,
            &ConfigFormat::Json {
                servers_key: "mcpServers",
            },
            "0",
        );
        assert_eq!(other["env"]["TILTH_MCP_CWD_HOOK_INJECTED"], json!("0"));

        let local = tilth_server_entry(true, &ConfigFormat::JsonLocal { servers_key: "mcp" }, "0");
        assert_eq!(
            local["environment"]["TILTH_MCP_CWD_HOOK_INJECTED"],
            json!("0")
        );
    }

    /// The TOML config path (codex — the host that most depends on the
    /// explicit-cwd posture) emits the hook-injected env var as an inline
    /// table that parses as valid TOML with the right value.
    #[test]
    fn toml_section_carries_hook_injected_env() {
        for hook_injected in ["0", "1"] {
            let (command, args) = tilth_command_and_args(false);
            let table = build_tilth_toml_table(&command, &args, hook_injected);
            let mut doc = toml_edit::DocumentMut::new();
            insert_tilth_table(doc.as_table_mut(), table).unwrap();
            let parsed: toml::Table = doc
                .to_string()
                .parse()
                .expect("generated [mcp_servers.tilth] section must be valid TOML");
            assert_eq!(
                parsed["mcp_servers"]["tilth"]["env"]["TILTH_MCP_CWD_HOOK_INJECTED"]
                    .as_str()
                    .expect("env var must be a TOML string"),
                hook_injected,
                "TOML env emission must carry the hook-injected flag"
            );
        }
    }

    /// Upserting the hook twice must yield exactly one `mcp__tilth__.*`
    /// `PreToolUse` entry — `tilth install claude-code` run twice should not
    /// duplicate the hook.
    #[test]
    fn pretooluse_hook_upsert_is_idempotent() {
        let mut settings = json!({});
        upsert_pretooluse_hook(&mut settings, "/home/x/.claude/tilth/inject-cwd.js").unwrap();
        upsert_pretooluse_hook(&mut settings, "/home/x/.claude/tilth/inject-cwd.js").unwrap();

        let entries = settings["hooks"]["PreToolUse"].as_array().unwrap();
        let tilth_entries: Vec<_> = entries
            .iter()
            .filter(|e| e["matcher"] == json!(TILTH_HOOK_MATCHER))
            .collect();
        assert_eq!(
            tilth_entries.len(),
            1,
            "expected exactly one tilth PreToolUse entry, got: {entries:?}"
        );
    }

    /// An existing unrelated `PreToolUse` entry and an unrelated top-level
    /// settings key must both survive the upsert.
    #[test]
    fn pretooluse_hook_upsert_preserves_unrelated_entries() {
        let mut settings = json!({
            "otherTopLevelSetting": true,
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo hi" }] }
                ],
                "PostToolUse": [
                    { "matcher": "Edit", "hooks": [{ "type": "command", "command": "echo bye" }] }
                ]
            }
        });
        upsert_pretooluse_hook(&mut settings, "/home/x/.claude/tilth/inject-cwd.js").unwrap();

        assert_eq!(settings["otherTopLevelSetting"], json!(true));
        let entries = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert!(
            entries.iter().any(|e| e["matcher"] == json!("Bash")),
            "unrelated PreToolUse entry was dropped: {entries:?}"
        );
        assert_eq!(
            settings["hooks"]["PostToolUse"][0]["matcher"],
            json!("Edit"),
            "unrelated hook event was dropped"
        );
    }

    /// The upserted entry has the right matcher, command type, and a `node`
    /// invocation of the written script path.
    #[test]
    fn pretooluse_hook_entry_shape() {
        let mut settings = json!({});
        upsert_pretooluse_hook(&mut settings, "/home/x/.claude/tilth/inject-cwd.js").unwrap();

        let entry = &settings["hooks"]["PreToolUse"][0];
        assert_eq!(entry["matcher"], json!(TILTH_HOOK_MATCHER));
        let command = entry["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(entry["hooks"][0]["type"], json!("command"));
        assert!(command.starts_with("node "), "command: {command}");
        assert!(
            command.contains("/home/x/.claude/tilth/inject-cwd.js"),
            "command should contain the script path: {command}"
        );
    }
}
