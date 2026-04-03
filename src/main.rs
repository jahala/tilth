use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process;

use clap::{CommandFactory, Parser};
use clap_complete::Shell;

/// tilth — Tree-sitter indexed lookups, smart code reading for AI agents.
/// One tool replaces `read_file`, grep, glob, `ast_grep`, and find.
#[derive(Parser)]
#[command(name = "tilth", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// File path, symbol name, glob pattern, or text to search.
    query: Option<String>,

    /// Directory to search within or resolve relative paths against.
    #[arg(long, default_value = ".")]
    scope: PathBuf,

    /// Line range or markdown heading (e.g. "45-89" or "## Architecture"). Bypasses smart view.
    #[arg(long)]
    section: Option<String>,

    /// Max tokens in response. Reduces detail to fit.
    #[arg(long)]
    budget: Option<u64>,

    /// Force full output (override smart view).
    #[arg(long)]
    full: bool,

    /// Machine-readable JSON output.
    #[arg(long)]
    json: bool,

    /// Run as MCP server (JSON-RPC on stdio).
    #[arg(long)]
    mcp: bool,

    /// Enable edit mode: hashline output + tilth_edit tool.
    #[arg(long)]
    edit: bool,

    /// Generate a structural codebase map.
    #[arg(long)]
    map: bool,

    /// Print shell completions for the given shell.
    #[arg(long, value_name = "SHELL")]
    completions: Option<Shell>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Install tilth into an MCP host's config.
    /// Supported hosts: claude-code, cursor, windsurf, vscode, claude-desktop, opencode
    Install {
        /// MCP host to configure.
        host: String,

        /// Enable edit mode (hashline output + tilth_edit tool).
        #[arg(long)]
        edit: bool,
    },
    /// Hook for Claude Code PreToolUse — rewrites Bash commands through tilth run.
    ///
    /// Reads tool input JSON from stdin, rewrites compressible commands
    /// (build/test/lint) to use `tilth run`, writes modified JSON to stdout.
    HookRewrite,
    /// Run a shell command with structured output compression.
    Run {
        /// The command to execute (passed to sh -c).
        #[arg(trailing_var_arg = true)]
        command: Vec<String>,

        /// Working directory for the command.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,

        /// Timeout in seconds.
        #[arg(long, default_value_t = 120)]
        timeout: u64,

        /// Output format: markdown, plain, json.
        #[arg(long)]
        format: Option<String>,
    },
}

fn main() {
    configure_thread_pools();
    let cli = Cli::parse();

    // Shell completions
    if let Some(shell) = cli.completions {
        clap_complete::generate(shell, &mut Cli::command(), "tilth", &mut io::stdout());
        return;
    }

    // Subcommands
    if let Some(cmd) = cli.command {
        match cmd {
            Command::Install { ref host, edit } => {
                if let Err(e) = tilth::install::run(host, edit) {
                    eprintln!("install error: {e}");
                    process::exit(1);
                }
            }
            Command::HookRewrite => {
                handle_hook_rewrite();
            }
            Command::Run {
                command,
                cwd,
                timeout,
                format,
            } => {
                handle_run(command, cwd, timeout, format);
            }
        }
        return;
    }

    // MCP mode: JSON-RPC server
    if cli.mcp {
        if let Err(e) = tilth::mcp::run(cli.edit) {
            eprintln!("mcp error: {e}");
            process::exit(1);
        }
        return;
    }

    let is_tty = io::stdout().is_terminal();

    // Map mode
    if cli.map {
        let cache = tilth::cache::OutlineCache::new();
        let scope = cli.scope.canonicalize().unwrap_or(cli.scope);
        let output = tilth::map::generate(&scope, 3, cli.budget, &cache);
        emit_output(&output, is_tty);
        return;
    }

    // CLI mode: single query
    let query = if let Some(q) = cli.query {
        q
    } else {
        eprintln!("usage: tilth <query> [--scope DIR] [--section N-M] [--budget N]");
        process::exit(3);
    };

    let cache = tilth::cache::OutlineCache::new();
    let scope = cli.scope.canonicalize().unwrap_or(cli.scope);

    // When piped (not a TTY), force full output — scripts expect raw content
    let full = cli.full || !is_tty;

    let result = if full {
        tilth::run_full(&query, &scope, cli.section.as_deref(), cli.budget, &cache)
    } else {
        tilth::run(&query, &scope, cli.section.as_deref(), cli.budget, &cache)
    };

    match result {
        Ok(output) => {
            if cli.json {
                let json = serde_json::json!({
                    "query": query,
                    "output": output,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json)
                        .expect("serde_json::Value is always serializable")
                );
            } else {
                emit_output(&output, is_tty);
            }
        }
        Err(e) => {
            eprintln!("{e}");
            process::exit(e.exit_code());
        }
    }
}

/// Hook rewrite: reads Bash tool input from stdin, rewrites compressible commands to use tilth run.
fn handle_hook_rewrite() -> ! {
    use io::Read;

    let mut input = String::new();
    if io::stdin().read_to_string(&mut input).is_err() || input.is_empty() {
        // Can't read stdin — pass through (exit 0 = no modification)
        process::exit(0);
    }

    let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&input) else {
        process::exit(0);
    };

    let Some(command) = json
        .get("command")
        .and_then(|v| v.as_str())
        .map(String::from)
    else {
        process::exit(0);
    };

    if should_rewrite_for_hook(&command) {
        // Find tilth binary path for the rewrite
        let tilth_bin = std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(String::from))
            .unwrap_or_else(|| "tilth".into());

        let rewritten = format!("{tilth_bin} run -- {command}");
        json["command"] = serde_json::Value::String(rewritten);
    }

    // Write modified (or unmodified) JSON to stdout
    let _ = io::stdout().write_all(serde_json::to_string(&json).unwrap_or(input).as_bytes());
    process::exit(0);
}

/// Check if a command should be routed through `tilth run` for compression.
fn should_rewrite_for_hook(command: &str) -> bool {
    let cmd = command.trim();

    // Already wrapped
    if cmd.starts_with("tilth") {
        return false;
    }

    // Extract the base command name (handle paths like /usr/bin/cargo)
    let first_word = cmd.split_whitespace().next().unwrap_or("");
    let base = first_word.rsplit('/').next().unwrap_or(first_word);

    matches!(
        base,
        "cargo"
            | "rustc"
            | "npm"
            | "npx"
            | "pnpm"
            | "yarn"
            | "bun"
            | "pip"
            | "pip3"
            | "pytest"
            | "python"
            | "python3"
            | "go"
            | "tsc"
            | "eslint"
            | "ruff"
            | "mypy"
            | "flake8"
            | "pylint"
            | "make"
            | "cmake"
            | "gradle"
            | "mvn"
            | "jest"
            | "vitest"
            | "mocha"
            | "dotnet"
            | "msbuild"
            | "gcc"
            | "g++"
            | "clang"
            | "clang++"
    )
}

/// Execute a shell command, compress its output, and exit with the command's exit code.
fn handle_run(command: Vec<String>, cwd: PathBuf, timeout: u64, format: Option<String>) -> ! {
    use tilth::run::{exec, process_structured, OutputFormat};

    let is_tty = io::stdout().is_terminal();

    if command.is_empty() {
        eprintln!("error: no command given");
        process::exit(1);
    }

    let cmd_str = shell_join(&command);
    let cwd = cwd.canonicalize().unwrap_or(cwd);

    let exec_result = match exec::execute(&cmd_str, &cwd, timeout) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("run error: {e}");
            process::exit(1);
        }
    };

    if exec_result.timed_out {
        eprintln!("command timed out after {timeout}s");
        let partial = match (exec_result.stdout.is_empty(), exec_result.stderr.is_empty()) {
            (_, true) => exec_result.stdout,
            (true, _) => exec_result.stderr,
            _ => format!("{}{}", exec_result.stdout, exec_result.stderr),
        };
        if !partial.is_empty() {
            emit_output(&partial, is_tty);
        }
        process::exit(124);
    }

    let combined = match (exec_result.stdout.is_empty(), exec_result.stderr.is_empty()) {
        (_, true) => exec_result.stdout,
        (true, _) => exec_result.stderr,
        _ => format!("{}{}", exec_result.stdout, exec_result.stderr),
    };

    let fmt = match format.as_deref() {
        Some("json") => OutputFormat::Json,
        Some("markdown") => OutputFormat::Markdown,
        Some("plain") => OutputFormat::Plain,
        None if is_tty => OutputFormat::Plain,
        _ => OutputFormat::Markdown,
    };

    let result = process_structured(&combined);

    let output = if result.passthrough {
        combined.clone()
    } else {
        result
            .format_checked(fmt, combined.len())
            .unwrap_or(combined)
    };

    if exec_result.exit_code != 0 {
        eprintln!("exit code: {}", exec_result.exit_code);
    }

    emit_output(&output, is_tty);
    process::exit(exec_result.exit_code);
}

/// Join command arguments, quoting any that contain shell metacharacters.
fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if arg.is_empty() {
                "''".to_string()
            } else if arg.contains(|c: char| {
                c.is_ascii_whitespace() || "\"'\\$`!#&|;(){}[]<>?*~".contains(c)
            }) {
                // Single-quote the arg, escaping any embedded single quotes
                format!("'{}'", arg.replace('\'', "'\\''"))
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Write output to stdout. When TTY and output is long, pipe through $PAGER.
fn emit_output(output: &str, is_tty: bool) {
    let line_count = output.lines().count();
    let term_height = terminal_height();

    if is_tty && line_count > term_height {
        let pager = std::env::var("PAGER").unwrap_or_else(|_| "less".into());
        if let Ok(mut child) = process::Command::new(&pager)
            .arg("-R")
            .stdin(process::Stdio::piped())
            .spawn()
        {
            if let Some(ref mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(output.as_bytes());
            }
            let _ = child.wait();
            return;
        }
    }

    println!("{output}");
}

fn terminal_height() -> usize {
    // Try LINES env var first (set by some shells)
    if let Ok(lines) = std::env::var("LINES") {
        if let Ok(h) = lines.parse::<usize>() {
            return h;
        }
    }
    // Fallback
    24
}

/// Configure rayon global thread pool to limit CPU usage.
///
/// Defaults to min(cores / 2, 6). Override with `TILTH_THREADS` env var.
/// This matters for long-lived MCP sessions where back-to-back searches
/// can sustain high CPU (see #27).
fn configure_thread_pools() {
    let num_threads = std::env::var("TILTH_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).clamp(2, 6))
        });

    rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads)
        .build_global()
        .ok();
}
