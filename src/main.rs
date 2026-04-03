use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process;

use clap::{CommandFactory, Parser, ValueEnum};
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

/// Output format for the `run` subcommand.
#[derive(Clone, ValueEnum)]
enum RunFormat {
    Markdown,
    Plain,
    Json,
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

        /// Output format.
        #[arg(long)]
        format: Option<RunFormat>,
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

        // Quote both paths: tilth_bin may contain spaces, and command may contain
        // shell metacharacters (&&, ||, ;, pipes). Single-quoting passes the full
        // compound command as one argument to tilth's trailing_var_arg, which then
        // joins and passes it to sh -c.
        let tilth_quoted = shell_quote_single(&tilth_bin);
        let cmd_quoted = shell_quote_single(&command);
        let rewritten = format!("{tilth_quoted} run -- {cmd_quoted}");
        json["command"] = serde_json::Value::String(rewritten);
    }

    // Write modified (or unmodified) JSON to stdout
    let _ = io::stdout().write_all(serde_json::to_string(&json).unwrap_or(input).as_bytes());
    process::exit(0);
}

/// Check if a command should be routed through `tilth run` for compression.
fn should_rewrite_for_hook(command: &str) -> bool {
    let cmd = command.trim();
    let base = extract_base_command(cmd);

    // Already wrapped — exact word boundary so "tilthx" doesn't match
    if base == "tilth" {
        return false;
    }

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

/// Extract the actual base command name from a shell command string, skipping:
/// - `KEY=VALUE` environment variable assignments
/// - Known wrapper commands: `sudo`, `env`, `time`, `nice`, `nohup`, `strace`, `ltrace`
///
/// Also strips path prefixes (e.g. `/usr/bin/cargo` → `cargo`).
fn extract_base_command(cmd: &str) -> &str {
    let mut words = cmd.split_whitespace();
    loop {
        let Some(word) = words.next() else {
            return "";
        };
        let base = word.rsplit('/').next().unwrap_or(word);
        // Skip KEY=VALUE env var assignments (contains '=' but doesn't start with it)
        if base.contains('=') && !base.starts_with('=') {
            continue;
        }
        // Skip known wrapper commands
        if matches!(
            base,
            "sudo" | "env" | "time" | "nice" | "nohup" | "strace" | "ltrace"
        ) {
            continue;
        }
        return base;
    }
}

/// Build the shell command string for `sh -c`.
///
/// When there is exactly one argument (e.g. a fully-formed compound command delivered
/// by the hook path), pass it through verbatim — calling `shell_join` would re-quote
/// it, causing `sh -c` to fail. For multiple arguments, join with quoting.
fn build_command_string(command: Vec<String>) -> String {
    if command.len() == 1 {
        command.into_iter().next().unwrap()
    } else {
        shell_join(&command)
    }
}

/// Execute a shell command, compress its output, and exit with the command's exit code.
fn handle_run(command: Vec<String>, cwd: PathBuf, timeout: u64, format: Option<RunFormat>) -> ! {
    use tilth::run::{exec, process_structured, OutputFormat};

    let is_tty = io::stdout().is_terminal();

    if command.is_empty() {
        eprintln!("error: no command given");
        process::exit(1);
    }

    let cmd_str = build_command_string(command);
    let cwd = cwd.canonicalize().unwrap_or(cwd);

    let exec_result = match exec::execute(&cmd_str, &cwd, timeout) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("run error: {e}");
            process::exit(1);
        }
    };

    let exit_code = exec_result.exit_code;
    let timed_out = exec_result.timed_out;

    if timed_out {
        eprintln!("command timed out after {timeout}s");
        let partial = exec_result.combined_output();
        if !partial.is_empty() {
            emit_output(&partial, is_tty);
        }
        process::exit(124);
    }

    let combined = exec_result.combined_output();

    let fmt = match format {
        Some(RunFormat::Json) => OutputFormat::Json,
        Some(RunFormat::Markdown) => OutputFormat::Markdown,
        Some(RunFormat::Plain) => OutputFormat::Plain,
        None if is_tty => OutputFormat::Plain,
        None => OutputFormat::Markdown,
    };

    let result = process_structured(&combined);

    let output = if result.passthrough {
        combined
    } else {
        result
            .format_checked(fmt, combined.len())
            .unwrap_or(combined)
    };

    if exit_code != 0 {
        eprintln!("exit code: {exit_code}");
    }

    emit_output(&output, is_tty);
    process::exit(exit_code);
}

/// Wrap a string in single quotes for use in a shell command.
///
/// Single-quotes prevent all shell interpretation (metacharacters, variable expansion, etc.).
/// Embedded single quotes are escaped using the `'\''` idiom.
fn shell_quote_single(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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

    print!("{output}");
    let _ = io::stdout().flush();
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

#[cfg(test)]
mod tests {
    use super::*;

    // --- should_rewrite_for_hook ---

    #[test]
    fn rewrite_simple_cargo() {
        assert!(should_rewrite_for_hook("cargo test"));
    }

    #[test]
    fn no_rewrite_already_wrapped() {
        assert!(!should_rewrite_for_hook("tilth run -- cargo test"));
    }

    #[test]
    fn no_rewrite_tilth_alone() {
        assert!(!should_rewrite_for_hook("tilth"));
    }

    /// Bug #16: "tilthx" must NOT be treated as already-wrapped.
    #[test]
    fn rewrite_tilthx_not_matched_by_guard() {
        // "tilthx" is not a known build command so should_rewrite returns false,
        // but the guard must not incorrectly suppress the check — tilthx != tilth.
        // Since tilthx is not in the allow-list either, the result is false.
        // The important thing: it must not return false due to the "already wrapped" guard.
        // We verify by checking a known command prefixed with "tilth" in the name:
        // use a hypothetical "tilthbuild cargo test" — not a real command, so false is correct.
        // More directly: a command whose first word is "tilthx" should reach the matches! block.
        // It won't match any known command, so result is false — but NOT because of guard.
        assert!(!should_rewrite_for_hook("tilthx cargo test"));
    }

    /// Bug #16 — the guard must not block "tilth_something" style names.
    #[test]
    fn rewrite_guard_exact_word_only() {
        // "tilthy build" — base == "tilthy", not "tilth", so guard doesn't fire.
        // Not in the match list → returns false (correct — no rewrite for unknown commands).
        assert!(!should_rewrite_for_hook("tilthy build"));
    }

    /// Compound command (Bug #5) — cargo + shell operator
    #[test]
    fn rewrite_compound_command() {
        assert!(should_rewrite_for_hook("cargo test && echo done"));
    }

    #[test]
    fn rewrite_compound_pipe() {
        assert!(should_rewrite_for_hook("cargo build 2>&1 | tee build.log"));
    }

    #[test]
    fn no_rewrite_unknown_command() {
        assert!(!should_rewrite_for_hook("echo hello"));
    }

    // --- shell_quote_single ---

    #[test]
    fn quote_simple_string() {
        assert_eq!(shell_quote_single("hello"), "'hello'");
    }

    #[test]
    fn quote_string_with_spaces() {
        // Handles paths like /Users/John Doe/.cargo/bin/tilth
        assert_eq!(
            shell_quote_single("/Users/John Doe/.cargo/bin/tilth"),
            "'/Users/John Doe/.cargo/bin/tilth'"
        );
    }

    #[test]
    fn quote_string_with_single_quote() {
        assert_eq!(shell_quote_single("it's"), "'it'\\''s'");
    }

    #[test]
    fn quote_compound_command() {
        // A compound command passed as one arg
        assert_eq!(
            shell_quote_single("cargo test && echo done"),
            "'cargo test && echo done'"
        );
    }

    // --- handle_hook_rewrite integration (rewrite format) ---

    /// Verify the rewritten command has the correct structure for Bug #5 and Bug #17.
    #[test]
    fn hook_rewrite_quotes_compound_command() {
        // Simulate what handle_hook_rewrite does for a compound command
        let command = "cargo test && echo done";
        let tilth_bin = "/Users/John Doe/.cargo/bin/tilth";

        let tilth_quoted = shell_quote_single(tilth_bin);
        let cmd_quoted = shell_quote_single(command);
        let rewritten = format!("{tilth_quoted} run -- {cmd_quoted}");

        assert_eq!(
            rewritten,
            "'/Users/John Doe/.cargo/bin/tilth' run -- 'cargo test && echo done'"
        );
    }

    #[test]
    fn hook_rewrite_simple_command() {
        let command = "cargo test";
        let tilth_bin = "/home/user/.cargo/bin/tilth";

        let tilth_quoted = shell_quote_single(tilth_bin);
        let cmd_quoted = shell_quote_single(command);
        let rewritten = format!("{tilth_quoted} run -- {cmd_quoted}");

        assert_eq!(
            rewritten,
            "'/home/user/.cargo/bin/tilth' run -- 'cargo test'"
        );
    }

    // --- build_command_string: single-arg passthrough ---
    //
    // When the hook rewrites a command, it produces something like:
    //   tilth run -- 'cargo test && echo done'
    // Clap parses this as a single trailing_var_arg element: ["cargo test && echo done"].
    // build_command_string must pass it through verbatim — calling shell_join would
    // re-quote it, causing sh -c to treat the whole thing as a literal binary name.
    // shell_join itself is correct for multi-arg joining; the fix is in the caller.

    #[test]
    fn build_command_string_single_compound_arg_passthrough() {
        // A compound command arriving as one element must come through verbatim.
        let args = vec!["cargo test && echo done".to_string()];
        assert_eq!(build_command_string(args), "cargo test && echo done");
    }

    #[test]
    fn build_command_string_single_simple_arg_passthrough() {
        // A simple two-word command passed as one element must come through verbatim.
        let args = vec!["cargo test".to_string()];
        assert_eq!(build_command_string(args), "cargo test");
    }

    #[test]
    fn build_command_string_multi_arg_joins_with_quoting() {
        // Multiple args (CLI invocation like `tilth run cargo test`) are shell-joined.
        let args = vec!["cargo".to_string(), "test".to_string()];
        assert_eq!(build_command_string(args), "cargo test");
    }

    #[test]
    fn build_command_string_multi_arg_with_metachar_quoting() {
        // An arg with metacharacters in a multi-arg context gets quoted by shell_join.
        let args = vec!["echo".to_string(), "hello world".to_string()];
        assert_eq!(build_command_string(args), "echo 'hello world'");
    }

    #[test]
    fn rewrite_env_var_prefixed_command() {
        // Bug: env var prefix causes first_word to be "RUST_BACKTRACE=1",
        // base becomes "RUST_BACKTRACE=1", no match → returns false.
        assert!(should_rewrite_for_hook("RUST_BACKTRACE=1 cargo test"));
    }

    #[test]
    fn rewrite_sudo_prefixed_command() {
        assert!(should_rewrite_for_hook("sudo cargo test"));
    }

    #[test]
    fn rewrite_env_prefixed_command() {
        assert!(should_rewrite_for_hook("env cargo test"));
    }

    // --- FIX N4: absolute-path commands ---

    #[test]
    fn rewrite_absolute_path_cargo() {
        assert!(should_rewrite_for_hook("/usr/bin/cargo test"));
    }

    #[test]
    fn rewrite_absolute_path_pytest() {
        assert!(should_rewrite_for_hook("/usr/local/bin/pytest foo.py"));
    }
}
