use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
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

    /// Disable project fingerprint in MCP init.
    #[arg(long)]
    no_overview: bool,

    /// Walk gitignored files too. By default tilth honors per-repo `.gitignore`
    /// and `.tilthignore` overrides (gitignore syntax — use `!path` to
    /// re-include). Equivalent to `TILTH_NO_IGNORE=1`.
    #[arg(long)]
    no_ignore: bool,

    /// Expand top N search matches with inline source (default: 2 when flag present).
    #[arg(long, num_args = 0..=1, default_missing_value = "2", require_equals = true)]
    expand: Option<usize>,

    /// File pattern filter (e.g. "*.rs", "!*.test.ts", "*.{go,rs}").
    #[arg(long)]
    glob: Option<String>,

    /// Find all callers of a symbol.
    #[arg(long, conflicts_with_all = ["deps", "map", "edit"])]
    callers: bool,

    /// Analyze blast-radius dependencies of a file.
    #[arg(long, conflicts_with_all = ["callers", "map", "edit"])]
    deps: bool,

    /// Generate a structural codebase map.
    #[arg(long, conflicts_with_all = ["callers", "deps", "expand", "section", "full"])]
    map: bool,

    /// Print shell completions for the given shell.
    #[arg(long, value_name = "SHELL")]
    completions: Option<Shell>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Install tilth into an MCP host's config.
    /// Supported hosts: claude-code, cursor, windsurf, vscode, claude-desktop, opencode, gemini, codex, amp, droid, antigravity, zed, copilot-cli, augment, kiro, kilo-code, cline, roo-code, trae, qwen-code, crush, pi
    Install {
        /// MCP host to configure.
        host: String,

        /// Enable edit mode (hashline output + tilth_edit tool).
        #[arg(long)]
        edit: bool,
    },
    /// Show structural diff with function-level change summaries.
    Diff {
        /// Diff source: uncommitted (default), staged, or a git ref (e.g. HEAD~1, main..feat).
        #[arg(default_value = "uncommitted")]
        source: String,

        /// Restrict diff to a specific file or directory.
        #[arg(long)]
        scope: Option<String>,

        /// First file for file-to-file diff (requires --b).
        #[arg(long)]
        a: Option<PathBuf>,

        /// Second file for file-to-file diff (requires --a).
        #[arg(long)]
        b: Option<PathBuf>,

        /// Path to a .patch file to parse.
        #[arg(long)]
        patch: Option<PathBuf>,

        /// Git log range for per-commit summaries (e.g. HEAD~5..HEAD).
        #[arg(long)]
        log: Option<String>,

        /// Filter output to symbols or files matching this substring.
        #[arg(long)]
        search: Option<String>,

        /// Show blast-radius warnings for signature-changed symbols.
        #[arg(long)]
        blast: bool,

        /// Expand top N changed symbols with full source context.
        #[arg(long, default_value_t = 0)]
        expand: usize,

        /// Max tokens in response.
        #[arg(long, default_value_t = 10000)]
        budget: u64,
    },
    /// Show the project fingerprint (what MCP init would inject).
    Overview,
}

fn main() {
    configure_thread_pools();
    let cli = Cli::parse();

    // Propagate --no-ignore via env so it reaches every WalkBuilder site
    // (CLI runners, MCP server, indexer, map). Set unconditionally so
    // subprocess inheritance works the same way for `tilth --mcp`.
    if cli.no_ignore {
        std::env::set_var("TILTH_NO_IGNORE", "1");
    }

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
            Command::Overview => {
                let cwd = std::env::current_dir().unwrap_or_default();
                let output = tilth::overview::fingerprint(&cwd);
                if output.is_empty() {
                    eprintln!("No project fingerprint could be generated.");
                    process::exit(1);
                }
                println!("{output}");
            }
            Command::Diff {
                source,
                scope,
                a,
                b,
                patch,
                log,
                search,
                blast,
                expand,
                budget,
            } => {
                let a_str = a.as_ref().map(|p| p.to_string_lossy().into_owned());
                let b_str = b.as_ref().map(|p| p.to_string_lossy().into_owned());
                let patch_str = patch.as_ref().map(|p| p.to_string_lossy().into_owned());
                let diff_source = match tilth::diff::resolve_source(
                    Some(&source),
                    a_str.as_deref(),
                    b_str.as_deref(),
                    patch_str.as_deref(),
                    log.as_deref(),
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("diff error: {e}");
                        process::exit(1);
                    }
                };
                let budget_opt = if budget == 0 { None } else { Some(budget) };
                match tilth::diff::diff(
                    &diff_source,
                    scope.as_deref(),
                    search.as_deref(),
                    blast,
                    expand,
                    budget_opt,
                ) {
                    Ok(output) => emit_output(&output, io::stdout().is_terminal()),
                    Err(e) => {
                        eprintln!("diff error: {e}");
                        process::exit(1);
                    }
                }
            }
        }
        return;
    }

    // MCP mode: JSON-RPC server
    if cli.mcp {
        if cli.no_overview {
            std::env::set_var("TILTH_NO_OVERVIEW", "1");
        }
        // Pass --scope to MCP if it's not the default "."
        let mcp_scope = if cli.scope.as_os_str() == "." {
            None
        } else {
            Some(
                cli.scope
                    .canonicalize()
                    .unwrap_or_else(|_| cli.scope.clone()),
            )
        };
        if let Err(e) = tilth::mcp::run(cli.edit, mcp_scope.as_deref()) {
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
    let expand = cli.expand.unwrap_or(0);

    // Callers mode
    if cli.callers {
        let result = tilth::run_callers(
            &query,
            &scope,
            expand,
            cli.budget,
            cli.glob.as_deref(),
            &cache,
        );
        emit_result(result, &query, cli.json, is_tty);
        return;
    }

    // Deps mode
    if cli.deps {
        let path = if Path::new(&query).is_absolute() {
            PathBuf::from(&query)
        } else {
            let scope_path = scope.join(&query);
            if scope_path.exists() {
                scope_path
            } else {
                let cwd_path = std::env::current_dir().unwrap_or_default().join(&query);
                if cwd_path.exists() {
                    cwd_path
                } else {
                    scope_path // fall back, let analyze_deps report the error
                }
            }
        };
        let result = tilth::run_deps(&path, &scope, cli.budget, &cache);
        emit_result(result, &query, cli.json, is_tty);
        return;
    }

    let result = if expand > 0 {
        tilth::run_expanded(
            &query,
            &scope,
            cli.section.as_deref(),
            cli.budget,
            full,
            expand,
            cli.glob.as_deref(),
            &cache,
        )
    } else if full {
        tilth::run_full(
            &query,
            &scope,
            cli.section.as_deref(),
            cli.budget,
            cli.glob.as_deref(),
            &cache,
        )
    } else {
        tilth::run(
            &query,
            &scope,
            cli.section.as_deref(),
            cli.budget,
            cli.glob.as_deref(),
            &cache,
        )
    };

    emit_result(result, &query, cli.json, is_tty);
}

fn emit_result(
    result: Result<String, tilth::error::TilthError>,
    query: &str,
    json: bool,
    is_tty: bool,
) {
    match result {
        Ok(output) => {
            if json {
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
