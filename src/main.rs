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

    /// Line range (e.g. "45-89"). Only for file reads. Bypasses smart view.
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
    /// Supported hosts: claude-code, cursor, windsurf, vscode, claude-desktop
    Install {
        /// MCP host to configure.
        host: String,

        /// Enable edit mode (hashline output + tilth_edit tool).
        #[arg(long)]
        edit: bool,
    },
}

fn main() {
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
