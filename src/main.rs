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
    scope: Vec<PathBuf>,

    /// Respect .gitignore, .ignore, and git exclude files while walking.
    #[arg(long)]
    respect_gitignore: bool,

    /// Line range or markdown heading (e.g. "45-89" or "## Architecture"). Bypasses smart view.
    #[arg(long)]
    section: Option<String>,

    /// Max tokens in response. Reduces detail to fit.
    #[arg(long)]
    budget: Option<u64>,

    /// Force full output (effect depends on query type — see --help).
    ///
    /// File path: return the whole file instead of an outline (bypass smart view).
    ///
    /// Symbol / text / regex: inline source for every match (equivalent to
    /// `--expand=<all>`). Explicit `--expand=N` wins. Output stays bounded
    /// by `--budget`.
    ///
    /// Glob: no effect (glob queries already return a flat file list).
    #[arg(long)]
    full: bool,

    /// Machine-readable JSON output.
    #[arg(long)]
    json: bool,

    /// Run as MCP server (JSON-RPC on stdio).
    #[arg(long)]
    mcp: bool,

    /// Enable edit mode: hashline output + tilth_write tool.
    #[arg(long)]
    edit: bool,

    /// Disable project fingerprint in MCP init.
    #[arg(long)]
    no_overview: bool,

    /// Inline source for top N search matches (default 2 when flag bare).
    ///
    /// Applies to symbol / text / regex queries. Without the flag the
    /// result is just the outline summary. `--full` upgrades this to
    /// expand every match (subject to `--budget`); explicit `--expand=N`
    /// wins over `--full`. No effect on file-path or glob queries.
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

        /// Enable edit mode (hashline output + tilth_write tool).
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
    /// Grok a symbol — one call returns def + doc + callees + callers + siblings + tests.
    ///
    /// Target accepts: bare symbol (`parse_unified_diff`), path:line
    /// (`src/diff/parse.rs:7`), or qualified name (`Type::method`).
    Grok {
        /// Symbol name or path:line.
        target: String,

        /// Restrict search to a subdirectory.
        #[arg(long, default_value = ".")]
        scope: PathBuf,

        /// Widen output caps (more callers, callees, siblings, tests).
        #[arg(long)]
        full: bool,
    },
}

fn main() {
    configure_thread_pools();
    let cli = Cli::parse();

    if cli.respect_gitignore {
        std::env::set_var("TILTH_RESPECT_GITIGNORE", "1");
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
            Command::Grok {
                target,
                scope,
                full,
            } => {
                let scope = scope.canonicalize().unwrap_or(scope);
                match tilth::run_grok(&target, &scope, full) {
                    Ok(output) => emit_output(&output, io::stdout().is_terminal()),
                    Err(e) => {
                        eprintln!("grok error: {e}");
                        process::exit(e.exit_code());
                    }
                }
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
        let mcp_scope = if scopes_are_default(&cli.scope) {
            None
        } else if cli.scope.len() == 1 {
            Some(
                cli.scope[0]
                    .canonicalize()
                    .unwrap_or_else(|_| cli.scope[0].clone()),
            )
        } else {
            eprintln!("mcp mode accepts only one --scope");
            process::exit(2);
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
        let scopes = resolve_scopes(&cli.scope);
        let output = run_for_scopes(&scopes, |scope| {
            Ok(tilth::map::generate(scope, 3, cli.budget, &cache))
        });
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
    let scopes = resolve_scopes(&cli.scope);

    // When piped (not a TTY), force full output — scripts expect raw content.
    // This promotion exists for FilePath queries (return full file instead of
    // outline) and is harmless for Glob (which ignores `full`). Search queries
    // also receive `full=true` here but stay outline-only — they do not auto-
    // expand on piping. See the `cli.full` guard on the expand override below.
    let full = cli.full || !is_tty;

    // Explicit `--full` on a search query means expand every match. Guarded on
    // `cli.full` (NOT the piped-derived `full` above) so that subprocess /
    // pipeline callers (Claude Code's Bash tool, CI scripts, `tilth foo | rg`)
    // still receive the concise outline they want. They opt into expand-all by
    // adding `--full` themselves. Explicit `--expand=N` still wins because it
    // produces `expand != 0`. We over-apply to all query types — `run_inner`
    // only forwards `expand` to search dispatches, so the value is silently
    // ignored for FilePath and Glob.
    //
    let expand = compute_expand(cli.expand, cli.full);

    // Callers mode
    if cli.callers {
        let result = run_query_for_scopes(&scopes, &query, |scope| {
            tilth::run_callers(
                &query,
                scope,
                expand,
                cli.budget,
                cli.glob.as_deref(),
                cli.full,
            )
        });
        emit_result(result, &query, cli.json, is_tty);
        return;
    }

    // Deps mode
    if cli.deps {
        let path = resolve_query_path(&query, &scopes);
        let result = run_query_for_scopes(&scopes, &query, |scope| {
            tilth::run_deps(&path, scope, cli.budget)
        });
        emit_result(result, &query, cli.json, is_tty);
        return;
    }

    let result = run_query_for_scopes(&scopes, &query, |scope| {
        if expand > 0 {
            tilth::run_expanded(
                &query,
                scope,
                cli.section.as_deref(),
                cli.budget,
                full,
                expand,
                cli.glob.as_deref(),
                &cache,
                cli.full,
            )
        } else if full {
            tilth::run_full(
                &query,
                scope,
                cli.section.as_deref(),
                cli.budget,
                cli.glob.as_deref(),
                &cache,
            )
        } else {
            tilth::run(
                &query,
                scope,
                cli.section.as_deref(),
                cli.budget,
                cli.glob.as_deref(),
                &cache,
            )
        }
    });

    emit_result(result, &query, cli.json, is_tty);
}

fn scopes_are_default(scopes: &[PathBuf]) -> bool {
    scopes.len() == 1 && scopes[0].as_os_str() == "."
}

fn resolve_scopes(scopes: &[PathBuf]) -> Vec<PathBuf> {
    if scopes.is_empty() {
        return vec![PathBuf::from(".")
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from("."))];
    }

    scopes
        .iter()
        .map(|scope| scope.canonicalize().unwrap_or_else(|_| scope.clone()))
        .collect()
}

fn resolve_query_path(query: &str, scopes: &[PathBuf]) -> PathBuf {
    if Path::new(query).is_absolute() {
        return PathBuf::from(query);
    }

    for scope in scopes {
        let scoped = scope.join(query);
        if scoped.exists() {
            return scoped;
        }
    }

    let cwd_path = std::env::current_dir().unwrap_or_default().join(query);
    if cwd_path.exists() {
        return cwd_path;
    }

    scopes
        .first()
        .map_or_else(|| PathBuf::from(query), |scope| scope.join(query))
}

fn run_for_scopes<F>(scopes: &[PathBuf], mut run: F) -> String
where
    F: FnMut(&Path) -> Result<String, tilth::error::TilthError>,
{
    if scopes.len() == 1 {
        return run(&scopes[0]).unwrap_or_else(|e| e.to_string());
    }

    scopes
        .iter()
        .map(|scope| {
            let output = run(scope).unwrap_or_else(|e| e.to_string());
            format!("# Scope: {}\n\n{output}", scope.display())
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n")
}

fn run_query_for_scopes<F>(
    scopes: &[PathBuf],
    query: &str,
    mut run: F,
) -> Result<String, tilth::error::TilthError>
where
    F: FnMut(&Path) -> Result<String, tilth::error::TilthError>,
{
    if scopes.len() == 1 {
        return run(&scopes[0]);
    }

    let mut outputs = Vec::new();
    let mut first_err = None;
    for scope in scopes {
        match run(scope) {
            Ok(output) => outputs.push(format!("# Scope: {}\n\n{output}", scope.display())),
            Err(err) => {
                if first_err.is_none() {
                    first_err = Some(err);
                }
            }
        }
    }

    if outputs.is_empty() {
        Err(
            first_err.unwrap_or_else(|| tilth::error::TilthError::NotFound {
                path: PathBuf::from(query),
                suggestion: None,
            }),
        )
    } else {
        Ok(outputs.join("\n\n---\n"))
    }
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

    // Conventional CLI output ends with a newline so the next shell prompt
    // starts on its own line. Most internal formatters terminate with `\n`,
    // but the search-result footer (e.g. `(~507 tokens)`) and a few other
    // paths do not — guard at the sink rather than auditing every formatter.
    print!("{output}");
    if !output.ends_with('\n') {
        println!();
    }
    let _ = io::stdout().flush();
}

fn terminal_height() -> usize {
    // ioctl(TIOCGWINSZ) on stdout — the real terminal height. Bash maintains
    // $LINES as an unexported shell variable, so most subprocesses (us included)
    // never receive it; relying on it alone made tilth assume 24 rows in any
    // typical interactive shell and page nearly every result. Fall back to
    // $LINES then to 24 only if the ioctl is unavailable (tests, exotic TTYs).
    if let Some((_, terminal_size::Height(h))) = terminal_size::terminal_size() {
        return h as usize;
    }
    if let Ok(lines) = std::env::var("LINES") {
        if let Ok(h) = lines.parse::<usize>() {
            return h;
        }
    }
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

/// Compute the effective `expand` value for a search query from the raw
/// CLI flags. Lifted out of `main` so the `--full` / `--expand` precedence
/// is unit-testable.
///
/// Precedence:
/// - Explicit `--expand=N` always wins (`cli_expand = Some(n)`), even alongside `--full`.
/// - Bare `--full` with no `--expand` → `FULL_EXPAND_CAP` (50).
/// - Neither flag → 0 (no expansion).
///
/// Critically, `cli_full` is the *parsed* `--full` flag, NOT the piped-derived
/// `full = cli.full || !is_tty` in `main`. Subprocess / pipeline callers
/// (Claude Code's Bash tool, CI scripts, `tilth foo | rg`) must keep the
/// concise outline by default; expand-all is opt-in via explicit `--full`.
fn compute_expand(cli_expand: Option<usize>, cli_full: bool) -> usize {
    /// `--budget` already bounds output, but `expand=usize::MAX` makes tilth
    /// compute the expanded source for every match before truncating —
    /// wasted parsing + rendering on pathological queries. 50 is well above
    /// any practical "show me everything that matters" case (MAX_MATCHES is
    /// 10 for symbol search anyway).
    const FULL_EXPAND_CAP: usize = 50;
    match (cli_expand, cli_full) {
        (Some(n), _) => n,
        (None, true) => FULL_EXPAND_CAP,
        (None, false) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin `--expand=N` precedence — explicit value always wins, including
    /// when combined with `--full`.
    #[test]
    fn explicit_expand_wins_over_full() {
        assert_eq!(compute_expand(Some(2), false), 2);
        assert_eq!(compute_expand(Some(2), true), 2);
        assert_eq!(compute_expand(Some(0), false), 0);
        assert_eq!(compute_expand(Some(0), true), 0);
        assert_eq!(compute_expand(Some(99), true), 99);
    }

    /// Pin `--full` → expand=50 when no explicit `--expand`.
    #[test]
    fn bare_full_promotes_to_full_expand_cap() {
        assert_eq!(compute_expand(None, true), 50);
    }

    /// Pin the default — neither flag means no expansion.
    #[test]
    fn neither_flag_means_zero_expand() {
        assert_eq!(compute_expand(None, false), 0);
    }

    /// Pin the regression that 16212fc was authored to prevent: a piped
    /// invocation (where `main` sets `full = !is_tty = true` for FilePath
    /// queries) must still receive `expand=0` here. `compute_expand` only
    /// sees the parsed `cli.full`, never the piped-derived bool — so a
    /// future refactor that conflates the two would have to change this
    /// function's signature, making the violation visible.
    #[test]
    fn piped_invocation_does_not_auto_expand() {
        // Simulating: user ran `tilth foo` (no --full) but stdout is piped.
        // `main` will set `full = !is_tty = true` for downstream FilePath
        // handling, but cli.full stays false. compute_expand must return 0.
        let cli_full = false; // user did NOT pass --full
        assert_eq!(compute_expand(None, cli_full), 0);
    }

    #[test]
    fn repeated_scope_flags_accumulate() {
        let cli = Cli::try_parse_from(["tilth", "foo", "--scope", "src", "--scope", "tests"])
            .expect("parse repeated scopes");
        assert_eq!(
            cli.scope,
            vec![PathBuf::from("src"), PathBuf::from("tests")]
        );
    }
}
