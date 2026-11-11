#![forbid(unsafe_code)]

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process;

use clap::{CommandFactory, Parser};
use clap_complete::Shell;
use tilth::{diag, FileScopeHint};

/// tilth — Tree-sitter indexed lookups, smart code reading for AI agents.
/// One tool replaces `read_file`, grep, glob, `ast_grep`, and find.
#[derive(Parser)]
#[command(name = "tilth", version, about)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "by design: clap derives the CLI from this struct, one bool per on/off flag"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// File path, symbol name, glob pattern, or text to search.
    query: Option<String>,

    /// Directory to search within or resolve relative paths against.
    #[arg(long, default_value = ".")]
    scope: PathBuf,

    /// Respect .gitignore, .ignore, and Git exclude files while walking.
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
    /// Symbol / text / regex: raise the match cap from 10 to 100 and inline
    /// source for the top 50 matches. Explicit `--expand=N` sets the
    /// inline-source count only and leaves the raised cap in place, so
    /// `--full --expand=0` lists up to 100 matches with no source — except
    /// on comma-separated multi-symbol queries, which always expand at
    /// least one match per symbol. Output stays bounded by `--budget`.
    ///
    /// `--callers`: raise the call-site cap from 10 to 100.
    ///
    /// `--deps`: list every dependent instead of the first 15.
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
    #[expect(
        clippy::doc_markdown,
        reason = "by design: clap prints this comment as --help text, where backticks would show"
    )]
    edit: bool,

    /// Disable project fingerprint in MCP init.
    #[arg(long)]
    no_overview: bool,

    /// Inline source for top N search matches (default 2 when flag bare).
    ///
    /// Applies to symbol / text / regex queries. Without the flag the
    /// result is just the outline summary. `--full` upgrades this to the
    /// top 50 matches (subject to `--budget`); explicit `--expand=N` wins
    /// over `--full`, but only for the inline source — `--full`'s raised
    /// match cap still stands. No effect on file-path or glob queries.
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
    /// Supported hosts: claude-code, cursor, windsurf, vscode, claude-desktop, opencode, gemini, codex, amp, droid, antigravity, antigravity-cli, zed, copilot-cli, augment, kiro, kilo-code, cline, roo-code, trae, qwen-code, crush, pi
    Install {
        /// MCP host to configure.
        host: String,

        /// Enable edit mode (hashline output + tilth_write tool).
        #[arg(long)]
        #[expect(
            clippy::doc_markdown,
            reason = "by design: clap prints this comment as --help text, where backticks would show"
        )]
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

        /// Show the changed lines of the first N changed symbols below the overview.
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

#[expect(
    clippy::too_many_lines,
    reason = "config and root handling move out of here in #307"
)]
fn main() {
    let cli = Cli::parse();

    if cli.respect_gitignore {
        std::env::set_var("TILTH_RESPECT_GITIGNORE", "1");
    }
    configure_thread_pools();

    // Shell completions
    if let Some(shell) = cli.completions {
        let mut script = Vec::new();
        clap_complete::generate(shell, &mut Cli::command(), "tilth", &mut script);
        write_stdout(&String::from_utf8_lossy(&script));
        return;
    }

    // Subcommands
    if let Some(cmd) = cli.command {
        match cmd {
            Command::Install { ref host, edit } => {
                if let Err(e) = tilth::install::run(host, edit) {
                    diag::emit(format_args!("install error: {e}"));
                    process::exit(1);
                }
            }
            Command::Overview => {
                let cwd = std::env::current_dir().unwrap_or_else(|e| {
                    diag::emit(format_args!(
                        "overview error: cannot read the current directory: {e}"
                    ));
                    process::exit(1);
                });
                let output = tilth::overview::fingerprint(&cwd);
                if output.is_empty() {
                    diag::emit(format_args!("No project fingerprint could be generated."));
                    process::exit(1);
                }
                write_stdout(&output);
            }
            Command::Grok {
                target,
                scope,
                full,
            } => {
                let scope =
                    cli_scope(&scope, FileScopeHint::Directory).unwrap_or_else(|e| usage_exit(&e));
                match tilth::run_grok(&target, &scope, full) {
                    Ok(output) => emit_output(&output, stdout_is_terminal()),
                    Err(e) => {
                        diag::emit(format_args!("grok error: {e}"));
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
                        diag::emit(format_args!("diff error: {e}"));
                        process::exit(1);
                    }
                };
                let budget_opt = if budget == 0 { None } else { Some(budget) };
                match tilth::diff::diff(
                    &diff_source,
                    None, // repo: the CLI already runs in the caller's checkout
                    scope.as_deref(),
                    search.as_deref(),
                    blast,
                    expand,
                    budget_opt,
                ) {
                    Ok(output) => emit_output(&output, stdout_is_terminal()),
                    Err(e) => {
                        diag::emit(format_args!("diff error: {e}"));
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
        // Pass --scope to MCP if it's not the default ".". A scope that is
        // not a directory stops the launch: the server used to ignore it
        // and serve the directory it happened to start in.
        let mcp_scope = if cli.scope.as_os_str() == "." {
            None
        } else {
            Some(cli_scope(&cli.scope, FileScopeHint::Directory).unwrap_or_else(|e| usage_exit(&e)))
        };
        if let Err(e) = tilth::mcp::run(cli.edit, mcp_scope.as_deref()) {
            diag::emit(format_args!("mcp error: {e}"));
            process::exit(1);
        }
        return;
    }

    let is_tty = stdout_is_terminal();

    // Map mode
    if cli.map {
        let cache = tilth_core::cache::OutlineCache::new();
        let scope =
            cli_scope(&cli.scope, FileScopeHint::Directory).unwrap_or_else(|e| usage_exit(&e));
        let output = tilth::map::generate(&scope, 3, cli.budget, &cache);
        emit_output(&output, is_tty);
        return;
    }

    // CLI mode: single query
    let Some(query) = cli.query else {
        diag::emit(format_args!(
            "usage: tilth <query> [--scope DIR] [--section N-M] [--budget N]"
        ));
        process::exit(3);
    };

    let cache = tilth_core::cache::OutlineCache::new();
    // --deps takes no --glob, so only the search modes can offer one.
    let hint = if cli.deps {
        FileScopeHint::Directory
    } else {
        FileScopeHint::Glob
    };
    let scope = cli_scope(&cli.scope, hint).unwrap_or_else(|e| usage_exit(&e));

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
        let result = tilth::run_callers(
            &query,
            &scope,
            expand,
            cli.budget,
            cli.glob.as_deref(),
            cli.full,
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
                // An unreadable working directory offers no second place to
                // look, so the scope path stands and analyze_deps reports it.
                match std::env::current_dir().map(|cwd| cwd.join(&query)) {
                    Ok(cwd_path) if cwd_path.exists() => cwd_path,
                    _ => scope_path,
                }
            }
        };
        let result = tilth::run_deps(&path, &scope, cli.budget, cli.full);
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
            cli.full,
        )
    } else if full {
        // The last argument must stay `cli.full`, never the `full` above: a
        // piped invocation reaches this branch with `full = !is_tty` and must
        // not have its search match cap raised.
        tilth::run_full(
            &query,
            &scope,
            cli.section.as_deref(),
            cli.budget,
            cli.glob.as_deref(),
            &cache,
            cli.full,
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

/// Canonicalize `--scope`, refusing a file the same way the MCP tools do.
/// A directory that does not exist is refused too, with the close existing
/// directories named. It used to walk nothing and answer "0 matches" with
/// exit 0, which read as a real result. The MCP tools fall back to `root`
/// with the same hint instead, because there a typo still finds the answer.
fn cli_scope(raw: &Path, hint: FileScopeHint) -> Result<PathBuf, String> {
    let scope = raw.canonicalize().unwrap_or_else(|_| raw.to_path_buf());
    tilth::refuse_file_scope(raw, &scope, hint)?;
    if !scope.is_dir() {
        return Err(format!(
            "scope \"{}\" is not a valid directory. {}",
            raw.display(),
            tilth::missing_scope_hint(raw, &scope, Path::new("."))
        ));
    }
    Ok(scope)
}

/// Report a usage error and exit with the invalid-input code.
fn usage_exit(msg: &str) -> ! {
    diag::emit(format_args!("{msg}"));
    process::exit(3);
}

fn emit_result(
    result: Result<String, tilth_core::TilthError>,
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
                // `{:#}` is serde_json's pretty printer, the same one
                // `to_string_pretty` uses, without its impossible error.
                write_stdout(&format!("{json:#}"));
            } else {
                emit_output(&output, is_tty);
            }
        }
        Err(e) => {
            diag::emit(format_args!("{e}"));
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
                // Quitting the pager before the end closes this pipe; the
                // person has seen what they wanted, so a failed write is no
                // error.
                let _ = stdin.write_all(output.as_bytes());
            }
            // Waiting only keeps the prompt from returning under the pager.
            // If the wait fails, the pager has already gone and the output
            // was shown; there is nothing left to do.
            let _ = child.wait();
            return;
        }
    }

    write_stdout(output);
}

/// Write a whole response to stdout, the one sink every CLI path prints
/// through. A reader that went away early (`tilth big.rs --full | head -1`)
/// ends the run quietly with exit 0, as Unix tools do. `print!` panicked
/// there instead and aborted with exit 134. Any other write failure, such
/// as a full disk, is reported and exits 1.
fn write_stdout(output: &str) {
    #[expect(
        clippy::disallowed_methods,
        reason = "by design: this is the CLI's one stdout sink"
    )]
    let stdout = io::stdout();
    if let Err(e) = write_output(&mut stdout.lock(), output) {
        if e.kind() == io::ErrorKind::BrokenPipe {
            process::exit(0);
        }
        diag::emit(format_args!("output error: {e}"));
        process::exit(1);
    }
}

/// Whether results go to a terminal, which decides the pager and the default
/// view. It reads stdout's kind and writes nothing.
fn stdout_is_terminal() -> bool {
    #[expect(
        clippy::disallowed_methods,
        reason = "by design: a query on stdout; writes go through write_stdout"
    )]
    let stdout = io::stdout();
    stdout.is_terminal()
}

/// Conventional CLI output ends with a newline so the next shell prompt
/// starts on its own line. Most internal formatters terminate with `\n`,
/// but the search-result footer (e.g. `(~507 tokens)`) and a few other
/// paths do not — guard at the sink rather than auditing every formatter.
fn write_output(writer: &mut impl Write, output: &str) -> io::Result<()> {
    writer.write_all(output.as_bytes())?;
    if !output.ends_with('\n') {
        writer.write_all(b"\n")?;
    }
    writer.flush()
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
    /// any practical "show me everything that matters" case (`MAX_MATCHES` is
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
    /// invocation (where `main` sets `full = !is_tty = true` for `FilePath`
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

    /// A sink that fails after `room` bytes, or on flush.
    struct FullSink {
        room: usize,
        fail_flush: bool,
    }

    impl Write for FullSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.room == 0 {
                return Err(io::ErrorKind::StorageFull.into());
            }
            let n = buf.len().min(self.room);
            self.room -= n;
            Ok(n)
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.fail_flush {
                return Err(io::ErrorKind::StorageFull.into());
            }
            Ok(())
        }
    }

    /// A failure in the body, the trailing newline or the flush reaches the
    /// caller, so `write_stdout` can report it instead of exiting 0 on a
    /// truncated response.
    #[test]
    fn write_output_returns_every_write_failure() {
        for (room, fail_flush) in [(0, false), (2, false), (4, false), (usize::MAX, true)] {
            let mut sink = FullSink { room, fail_flush };
            let err = write_output(&mut sink, "body").unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::StorageFull, "room {room}");
        }
    }

    #[test]
    fn write_output_ends_with_exactly_one_newline() {
        for output in ["body", "body\n"] {
            let mut bytes = Vec::new();
            write_output(&mut bytes, output).unwrap();
            assert_eq!(bytes, b"body\n");
        }
    }

    /// A mistyped `--scope` used to search nothing and print "0 matches"
    /// with exit 0. It is a usage error that names the close directory.
    #[test]
    fn cli_scope_refuses_a_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("binding")).unwrap();
        // A checkout: the hint scans for close names only inside one.
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let err = cli_scope(&dir.path().join("bindng"), FileScopeHint::Glob).unwrap_err();
        assert!(
            err.contains("is not a valid directory") && err.contains("binding\"?"),
            "unexpected refusal: {err}"
        );
    }

    /// #195: `--scope <file>` gets the same refusal as the MCP tools. It used
    /// to walk the lone file and print match headers with no file name.
    #[test]
    fn cli_scope_refuses_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.py");
        std::fs::write(&file, "").unwrap();
        let err = cli_scope(&file, FileScopeHint::Glob).unwrap_err();
        assert!(
            err.contains("is a file") && err.contains("glob to its name (\"x.py\")"),
            "unexpected refusal: {err}"
        );
        let ok = cli_scope(dir.path(), FileScopeHint::Glob).unwrap();
        assert_eq!(ok, dir.path().canonicalize().unwrap());
    }
}
