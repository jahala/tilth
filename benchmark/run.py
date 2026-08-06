#!/usr/bin/env python3
"""
Benchmark runner for tilth performance evaluation.

Executes `claude -p` for each combination of (task, mode, model, repetition).
Records token usage, cost, correctness, and tool usage to JSONL format.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from collections.abc import Mapping
from contextlib import contextmanager
from dataclasses import asdict
from datetime import datetime
from pathlib import Path
from typing import Optional

# Add parent directory to path for imports
sys.path.insert(0, str(Path(__file__).parent))

from config import (
    MODELS,
    MODES,
    REPOS,
    RUNNERS,
    SYSTEM_PROMPT,
    DEFAULT_MAX_BUDGET_USD,
    SYNTHETIC_REPO,
    RESULTS_DIR,
    DEFAULT_REPS,
    TILTH_MCP_CODEX_ARGS,
    TILTH_BIN,
    OPENCODE_CONFIGS,
    OPENCODE_CONFIG_HOME,
)
from parse import parse_stream_json, parse_codex_json, parse_opencode_json, tool_call_counts, extract_stream_error
from tasks import TASKS
from fixtures.reset import reset_repo, ensure_repo_clean


def _tilth_version() -> Optional[str]:
    """Get installed tilth version via `tilth --version`."""
    try:
        result = subprocess.run(
            [TILTH_BIN, "--version"],
            capture_output=True, text=True, timeout=5,
        )
        # Output: "tilth 0.2.1"
        return result.stdout.strip().removeprefix("tilth ") if result.returncode == 0 else None
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return None


def get_repo_path(repo_name: str) -> Path:
    """Resolve working directory for a task's repo."""
    if repo_name == "synthetic":
        return SYNTHETIC_REPO
    return REPOS[repo_name].path


_RUNTIME_ENV_KEYS = frozenset(
    {
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "TMPDIR",
        "TERM",
        "LANG",
        "LANGUAGE",
        "XDG_DATA_HOME",
    }
)
_PROVIDER_AUTH_PREFIXES = ("ANTHROPIC_", "OPENAI_", "OPENROUTER_")
_PROVIDER_AUTH_KEYS = frozenset({"CODEX_API_KEY"})


def build_runner_env(
    runner: str,
    *,
    opencode_config: Optional[str] = None,
    bare: bool = False,
    ambient: Optional[Mapping[str, str]] = None,
) -> dict[str, str]:
    """Build a minimal environment for one runner subprocess."""
    source = os.environ if ambient is None else ambient
    env = {
        key: value
        for key, value in source.items()
        if key in _RUNTIME_ENV_KEYS
        or key in _PROVIDER_AUTH_KEYS
        or key.startswith(_PROVIDER_AUTH_PREFIXES)
        or key.startswith("LC_")
    }

    tilth_dir = os.path.dirname(TILTH_BIN)
    if tilth_dir:
        env["PATH"] = tilth_dir + os.pathsep + env.get("PATH", "")

    if runner == "opencode" and opencode_config is not None:
        OPENCODE_CONFIG_HOME.mkdir(parents=True, exist_ok=True)
        env["OPENCODE_CONFIG"] = opencode_config
        env["XDG_CONFIG_HOME"] = str(OPENCODE_CONFIG_HOME)
        if bare:
            env["OPENCODE_DISABLE_DEFAULT_PLUGINS"] = "1"
            env["OPENCODE_DISABLE_PROJECT_CONFIG"] = "1"
            env["OPENCODE_DISABLE_CLAUDE_CODE"] = "1"
            env["OPENCODE_DISABLE_EXTERNAL_SKILLS"] = "1"

    return env


def _compact_tool_sequence(result):
    """Extract ordered tool call names + key args from all turns."""
    seq = []
    for turn in result.turns:
        for tc in turn.tool_calls:
            entry = {"name": tc.name}
            # Add compact args summary
            args = {}
            for k, v in tc.input.items():
                if k == "command":
                    args[k] = str(v)[:80]
                elif k == "file_path":
                    args[k] = str(v).split("/")[-1]  # filename only
                elif k in ("pattern", "query", "path", "scope", "kind", "section", "expand"):
                    args[k] = str(v)[:60]
                elif k in ("paths", "sections", "patterns") and isinstance(v, list):
                    # Batch-capable read/glob args — file or segment counts.
                    args[f"{k}_count"] = len(v)
                elif k == "files" and isinstance(v, list):
                    # tilth_edit: count files in the batch AND total hunks across files.
                    args["files_count"] = len(v)
                    args["edits_count"] = sum(
                        len(f.get("edits", [])) for f in v if isinstance(f, dict)
                    )
                # skip other large args
            if args:
                entry["args"] = args
            seq.append(entry)
    return seq


@contextmanager
def _agent_repo(repo_path: Path, hide_git: bool):
    """Yield an agent workspace without repository metadata when requested."""
    if not hide_git:
        yield repo_path
        return

    with tempfile.TemporaryDirectory(prefix="tilth-benchmark-") as temp_dir:
        workspace = Path(temp_dir) / repo_path.name
        shutil.copytree(
            repo_path,
            workspace,
            ignore=shutil.ignore_patterns(".git", ".git_hidden"),
        )
        yield workspace


def run_single(
    task_name: str,
    mode_name: str,
    model_name: str,
    repetition: int,
    verbose: bool = False,
    stream_log_path: Optional[Path] = None,
    bare: bool = False,
) -> dict:
    """Run one benchmark cell in the task's configured agent workspace."""
    task = TASKS[task_name]
    with _agent_repo(
        get_repo_path(task.repo),
        getattr(task, "hide_git", False),
    ) as repo_path:
        return _run_single_in_repo(
            task_name,
            mode_name,
            model_name,
            repetition,
            repo_path,
            verbose=verbose,
            stream_log_path=stream_log_path,
            bare=bare,
        )


def _run_single_in_repo(
    task_name: str,
    mode_name: str,
    model_name: str,
    repetition: int,
    repo_path: Path,
    verbose: bool = False,
    stream_log_path: Optional[Path] = None,
    bare: bool = False,
) -> dict:
    """Execute and grade one benchmark cell."""
    task = TASKS[task_name]
    mode = MODES[mode_name]
    model_id = MODELS[model_name]
    runner = RUNNERS[model_name]
    opencode_config: Optional[str] = None

    # Build command based on runner
    if runner == "codex":
        cmd = [
            "codex", "exec",
            "--json",
            "--full-auto",
            "--ephemeral",
            "-m", model_id,
        ]

        # Add MCP config for tilth modes
        if mode.mcp_config_path:
            cmd += TILTH_MCP_CODEX_ARGS

        # Codex has no --system-prompt, prepend to prompt
        full_prompt = f"{SYSTEM_PROMPT}\n\n{task.prompt}"
        cmd += ["--", full_prompt]

    elif runner == "opencode":
        opencode_config = OPENCODE_CONFIGS.get(mode_name)
        if opencode_config is None:
            raise RuntimeError(
                f"opencode runner has no config for mode '{mode_name}'. "
                f"Supported modes: {', '.join(sorted(OPENCODE_CONFIGS))}."
            )
        # opencode has no --system-prompt; prepend like codex. OPENCODE_CONFIG
        # (set below) selects the MCP servers; --dangerously-skip-permissions
        # keeps the headless run from blocking on tool-permission prompts.
        full_prompt = f"{SYSTEM_PROMPT}\n\n{task.prompt}"
        cmd = [
            "opencode", "run",
            "--format", "json",
            "--dir", str(repo_path),
            "--model", model_id,
            "--dangerously-skip-permissions",
        ]
        if bare:
            cmd.append("--pure")  # strip external plugins (claude --bare parity)
        cmd.append(full_prompt)

    else:  # claude
        cmd = [
            "claude", "-p",
            "--output-format", "stream-json",
            "--verbose",
            "--model", model_id,
            "--max-budget-usd", str(DEFAULT_MAX_BUDGET_USD),
            "--no-session-persistence",
            "--dangerously-skip-permissions",
            "--strict-mcp-config",
            "--system-prompt", SYSTEM_PROMPT + f"\nYour current working directory is: {repo_path}",
        ]

        # --bare strips slash commands, hooks, plugins, agents, and skills.
        # Opt in for a maximally stripped harness (e.g. measuring tilth in
        # isolation). It does not touch the built-in tool set, so the --tools
        # allowlist below governs Grep/Glob identically with or without --bare.
        if bare:
            cmd += ["--bare"]

        tools_list = list(mode.tools)

        # --tools "" disables all built-ins (tilth_forced); --tools "a,b,c" allowlists; absent = default
        if tools_list:
            cmd += ["--tools", ",".join(tools_list)]
        elif mode.mcp_config_path:
            cmd += ["--tools", ""]

        if mode.mcp_config_path:
            cmd += ["--mcp-config", mode.mcp_config_path]

        cmd += ["--", task.prompt]

    if verbose:
        print(f"    Running: {' '.join(cmd)}")

    # Build a fresh allowlist for every runner subprocess. Runner-specific
    # config is added only by the lane that consumes it.
    env = build_runner_env(
        runner,
        opencode_config=opencode_config,
        bare=bare,
    )
    start_time = time.time()

    if runner == "claude" and stream_log_path is not None:
        # Tee claude's stream-json stdout to disk line-by-line so the run is
        # tailable while in-flight. Keeps the in-memory string for the existing
        # parse path. Codex (single-object JSON) keeps the simple subprocess.run.
        stream_log_path.parent.mkdir(parents=True, exist_ok=True)
        proc = subprocess.Popen(
            cmd,
            cwd=str(repo_path),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            stdin=subprocess.DEVNULL,
            text=True,
            bufsize=1,  # line-buffered
            env=env,
        )
        assert proc.stdout is not None and proc.stderr is not None
        stderr_pipe = proc.stderr
        stdout_chunks: list[str] = []
        stderr_chunks: list[str] = []
        timed_out = False

        def _drain_stderr() -> None:
            stderr_chunks.append(stderr_pipe.read())

        def _kill_on_timeout() -> None:
            nonlocal timed_out
            timed_out = True
            proc.kill()

        stderr_thread = threading.Thread(target=_drain_stderr)
        stderr_thread.start()
        timer = threading.Timer(600, _kill_on_timeout)
        timer.start()
        try:
            with open(stream_log_path, "w") as logf:
                for line in proc.stdout:
                    logf.write(line)
                    logf.flush()
                    stdout_chunks.append(line)
            proc.wait()
            stderr_thread.join()
        finally:
            timer.cancel()
            proc.stdout.close()
            stderr_pipe.close()

        if timed_out:
            raise subprocess.TimeoutExpired(cmd, 600)

        result = subprocess.CompletedProcess(
            args=cmd,
            returncode=proc.returncode,
            stdout="".join(stdout_chunks),
            stderr="".join(stderr_chunks),
        )
    else:
        result = subprocess.run(
            cmd,
            cwd=str(repo_path),
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=600,
            env=env,
        )
    elapsed_ms = int((time.time() - start_time) * 1000)

    if result.returncode != 0:
        runner_name = {"codex": "codex exec", "opencode": "opencode run"}.get(runner, "claude -p")
        # The real cause (e.g. a provider ContentFilterError) streams as a late
        # {"type":"error",...} event; a head-truncated dump hides it. Surface it.
        detail = extract_stream_error(result.stdout) or f"stdout tail: {result.stdout[-600:]}"
        raise RuntimeError(
            f"{runner_name} failed with code {result.returncode}\n"
            f"stderr: {result.stderr}\n"
            f"{detail}"
        )

    # Parse output based on runner
    if runner == "codex":
        run_result = parse_codex_json(result.stdout, model_id)
    elif runner == "opencode":
        run_result = parse_opencode_json(result.stdout)
    else:
        run_result = parse_stream_json(result.stdout)
    run_result.task_name = task_name
    run_result.mode_name = mode_name
    run_result.model_name = model_name
    run_result.repetition = repetition

    # Override duration if needed (subprocess timing may be more accurate)
    if run_result.duration_ms == 0:
        run_result.duration_ms = elapsed_ms

    # Check correctness
    correct, reason = task.check_correctness(
        run_result.result_text,
        str(repo_path),
    )
    run_result.correct = correct
    run_result.correctness_reason = reason

    # Build tool call breakdown
    tool_breakdown = tool_call_counts(run_result)

    # Collect per-turn context and output token counts.
    per_turn_context = [turn.context_tokens for turn in run_result.turns]
    per_turn_output = [turn.output_tokens for turn in run_result.turns]
    total_context = sum(per_turn_context)
    task_source = asdict(task.source)

    # Return JSON-serializable dict
    return {
        "task": task_name,
        "repo": task.repo,
        "mode": mode_name,
        "model": model_id,
        "model_alias": model_name,
        "capability": task.capability,
        "source": task_source,
        "repetition": repetition,
        "tilth_version": _tilth_version() if "tilth" in mode_name else None,
        "num_turns": run_result.num_turns,
        "num_tool_calls": sum(tool_breakdown.values()),
        "tool_calls": tool_breakdown,
        "total_cost_usd": run_result.total_cost_usd,
        "duration_ms": run_result.duration_ms,
        "context_tokens": total_context,
        "output_tokens": run_result.total_output_tokens,
        "input_tokens": run_result.total_input_tokens,
        "cache_creation_tokens": run_result.total_cache_creation_tokens,
        "cache_creation_5m_tokens": sum(
            turn.cache_creation_5m_tokens for turn in run_result.turns
        ),
        "cache_creation_1h_tokens": sum(
            turn.cache_creation_1h_tokens for turn in run_result.turns
        ),
        "cache_read_tokens": run_result.total_cache_read_tokens,
        "per_turn_context_tokens": per_turn_context,
        "per_turn_output_tokens": per_turn_output,
        "per_turn_token_usage": [
            {
                "input_tokens": turn.input_tokens,
                "cache_creation_tokens": turn.cache_creation_tokens,
                "cache_creation_5m_tokens": turn.cache_creation_5m_tokens,
                "cache_creation_1h_tokens": turn.cache_creation_1h_tokens,
                "cache_read_tokens": turn.cache_read_tokens,
                "output_tokens": turn.output_tokens,
            }
            for turn in run_result.turns
        ],
        "correct": correct,
        "correctness_reason": reason,
        "result_text": run_result.result_text[:5000],
        "tool_sequence": _compact_tool_sequence(run_result),
    }


def parse_comma_list(value: str, valid_options: dict, name: str) -> list[str]:
    """Parse comma-separated list and validate against valid options."""
    if value.lower() == "all":
        return list(valid_options.keys())

    items = [item.strip() for item in value.split(",") if item.strip()]
    invalid = [item for item in items if item not in valid_options]
    if invalid:
        raise ValueError(
            f"Invalid {name}: {', '.join(invalid)}. "
            f"Valid options: {', '.join(valid_options.keys())}"
        )
    return items


def main():
    parser = argparse.ArgumentParser(
        description="Run tilth benchmarks",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  python run.py --models sonnet --reps 5 --tasks all --modes all
  python run.py --models haiku --reps 1 --tasks find_definition --modes baseline,tilth
  python run.py --models sonnet,opus --reps 3 --tasks find_definition,edit_task --modes tilth
        """,
    )

    parser.add_argument(
        "--models",
        default="sonnet",
        help="Comma-separated model names or 'all' (default: sonnet)",
    )
    parser.add_argument(
        "--reps",
        type=int,
        default=DEFAULT_REPS,
        help=f"Number of repetitions (default: {DEFAULT_REPS})",
    )
    parser.add_argument(
        "--tasks",
        default="all",
        help="Comma-separated task names or 'all' (default: all)",
    )
    parser.add_argument(
        "--modes",
        default="all",
        help="Comma-separated mode names or 'all' (default: all)",
    )
    parser.add_argument(
        "--repos",
        default="all",
        help="Comma-separated repo names or 'all' (default: all). "
             "Filters tasks to those targeting specified repos.",
    )
    parser.add_argument(
        "--verbose",
        action="store_true",
        help="Print detailed output for debugging",
    )
    parser.add_argument(
        "--bare",
        action="store_true",
        help="Strip the harness to built-in tools + the per-mode MCP config. "
             "claude: passes --bare (drops slash commands, hooks, plugins, "
             "agents, skills). "
             "opencode: redirects XDG_CONFIG_HOME + sets OPENCODE_DISABLE_* so "
             "the user's global opencode config (MCP servers, skills) can't "
             "leak into the baseline. Ignored for codex.",
    )

    args = parser.parse_args()

    # Parse and validate inputs
    try:
        models = parse_comma_list(args.models, MODELS, "models")
        tasks_list = parse_comma_list(args.tasks, TASKS, "tasks")
        modes = parse_comma_list(args.modes, MODES, "modes")
    except ValueError as e:
        parser.error(str(e))
        return

    # Verify every MCP server referenced by selected modes can actually spawn.
    # Catches stale absolute paths in mcp config files (e.g. /Users/<other-user>/...).
    for mode_name in modes:
        cfg_path = MODES[mode_name].mcp_config_path
        if not cfg_path:
            continue
        try:
            with open(cfg_path) as fp:
                mcp_cfg = json.load(fp)
        except (OSError, json.JSONDecodeError) as e:
            print(f"ERROR: cannot read MCP config {cfg_path} for mode '{mode_name}': {e}", file=sys.stderr)
            sys.exit(1)
        for server_name, server_cfg in mcp_cfg.get("mcpServers", {}).items():
            cmd_str = server_cfg.get("command", "")
            resolved = shutil.which(cmd_str) if "/" not in cmd_str else (cmd_str if os.path.isfile(cmd_str) and os.access(cmd_str, os.X_OK) else None)
            if not resolved:
                print(f"ERROR: MCP server '{server_name}' in {cfg_path} (mode '{mode_name}')", file=sys.stderr)
                print(f"       command '{cmd_str}' is not executable / not on PATH.", file=sys.stderr)
                print(f"       Fix the 'command' field in {cfg_path} or install the binary.", file=sys.stderr)
                sys.exit(1)
            # Smoke-test the binary with --version to catch broken installs.
            try:
                probe = subprocess.run([resolved, "--version"], capture_output=True, text=True, timeout=5)
                if probe.returncode != 0:
                    print(f"WARNING: MCP server '{server_name}' --version exited {probe.returncode}: {probe.stderr.strip()}", file=sys.stderr)
            except (FileNotFoundError, subprocess.TimeoutExpired) as e:
                print(f"ERROR: MCP server '{server_name}' at {resolved} failed to run: {e}", file=sys.stderr)
                sys.exit(1)

    # Filter tasks by repo
    if args.repos.lower() != "all":
        requested_repos = set(r.strip() for r in args.repos.split(",") if r.strip())
        tasks_list = [t for t in tasks_list if TASKS[t].repo in requested_repos]
        if not tasks_list:
            parser.error(f"No tasks found for repos: {args.repos}")

    # Validate synthetic repo exists (only if synthetic tasks are selected)
    if "synthetic" in set(TASKS[t].repo for t in tasks_list):
        if not SYNTHETIC_REPO.exists():
            print("ERROR: Synthetic repo not found.")
            print(f"Expected at: {SYNTHETIC_REPO}")
            print("Run setup.py to create the test repository:")
            print("  python benchmark/fixtures/setup.py")
            sys.exit(1)

    # Validate real-world repos exist (for selected tasks)
    selected_repos = set(TASKS[t].repo for t in tasks_list) - {"synthetic"}
    for repo_name in selected_repos:
        repo_path = REPOS[repo_name].path
        if not repo_path.exists():
            print(f"ERROR: Repo '{repo_name}' not cloned.")
            print(f"Expected at: {repo_path}")
            print("Run setup_repos.py to clone repositories:")
            print("  python benchmark/fixtures/setup_repos.py")
            sys.exit(1)

    # Clean real-world repos before starting (removes junk files from previous runs)
    for repo_name in selected_repos:
        repo_path = REPOS[repo_name].path
        ensure_repo_clean(repo_path, REPOS[repo_name].commit_sha)
        if args.verbose:
            print(f"Cleaned repo: {repo_name}")

    # Create results directory
    RESULTS_DIR.mkdir(exist_ok=True)

    # Create timestamped output file (include model name to avoid collisions
    # when multiple benchmark processes run in parallel)
    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    model_suffix = f"_{models[0]}" if len(models) == 1 else ""
    output_file = RESULTS_DIR / f"benchmark_{timestamp}{model_suffix}.jsonl"
    stream_log_dir = RESULTS_DIR / "streams" / timestamp

    # Print configuration summary
    print("=" * 70)
    print("tilth Benchmark Runner")
    print("=" * 70)
    print(f"Models:      {', '.join(models)}")
    print(f"Tasks:       {', '.join(tasks_list)}")
    print(f"Modes:       {', '.join(modes)}")
    repos_used = sorted(set(TASKS[t].repo for t in tasks_list))
    print(f"Repos:       {', '.join(repos_used)}")
    print(f"Repetitions: {args.reps}")
    print(f"Output:      {output_file}")
    print(f"Streams:     {stream_log_dir}/<cell>.jsonl  (tail -f for live agent output)")
    print("=" * 70)
    print()

    # Calculate total runs
    total_runs = len(tasks_list) * len(modes) * len(models) * args.reps
    current_run = 0

    # Track previous state for reset logic
    prev_task = None
    prev_mode = None

    # Main benchmark loop
    with open(output_file, "w") as f:
        for task_name in tasks_list:
            task = TASKS[task_name]

            for mode_name in modes:
                for model_name in models:
                    for rep in range(args.reps):
                        current_run += 1
                        run_id = f"{task_name}/{mode_name}/{model_name}/rep{rep}"

                        # Reset repo and apply mutations for tasks that have them
                        if task.mutations:
                            repo_path = get_repo_path(task.repo)
                            if task.repo == "synthetic":
                                if rep > 0 or mode_name != prev_mode or task_name != prev_task:
                                    if args.verbose:
                                        print(f"  Resetting synthetic repo...")
                                    reset_repo()
                            else:
                                # Real repos: always clean + re-mutate before each run
                                if args.verbose:
                                    print(f"  Resetting {task.repo}...")
                                ensure_repo_clean(repo_path, REPOS[task.repo].commit_sha)
                            # Apply mutations (if any) after clean state
                            if task.mutations:
                                if args.verbose:
                                    print(f"  Applying {len(task.mutations)} mutation(s)...")
                                task.apply_mutations(str(repo_path))
                        elif task.repo == "synthetic" and mode_name != prev_mode:
                            reset_repo()

                        prev_task = task_name
                        prev_mode = mode_name

                        # Print progress
                        print(f"[{current_run}/{total_runs}] {run_id}")

                        # Run benchmark
                        cell_slug = (
                            f"{current_run:02d}_{task_name}_{mode_name}"
                            f"_{model_name}_rep{rep}"
                        )
                        record_metadata = {
                            "task": task_name,
                            "mode": mode_name,
                            "model": MODELS[model_name],
                            "model_alias": model_name,
                            "capability": task.capability,
                            "source": asdict(task.source),
                            "repetition": rep,
                            "per_turn_output_tokens": [],
                        }

                        try:
                            result = run_single(
                                task_name,
                                mode_name,
                                model_name,
                                rep,
                                verbose=args.verbose,
                                stream_log_path=stream_log_dir / f"{cell_slug}.jsonl",
                                bare=args.bare,
                            )

                            # Write JSONL record
                            f.write(json.dumps(result) + "\n")
                            f.flush()

                            # Print status line
                            status = "✓" if result["correct"] else "✗"
                            print(
                                f"  {status} "
                                f"{result['num_turns']}t "
                                f"{result['context_tokens']:,}ctx "
                                f"{result['output_tokens']:,}out "
                                f"${result['total_cost_usd']:.4f} "
                                f"{result['duration_ms']:,}ms"
                            )

                            if not result["correct"]:
                                print(f"  → {result['correctness_reason']}")

                        except subprocess.TimeoutExpired:
                            print(f"  ✗ TIMEOUT (>600s)")
                            error_result = {
                                **record_metadata,
                                "error": "timeout",
                                "correct": False,
                                "correctness_reason": "Subprocess timed out",
                            }
                            f.write(json.dumps(error_result) + "\n")
                            f.flush()

                        except Exception as e:
                            print(f"  ✗ ERROR: {e}")
                            if args.verbose:
                                import traceback
                                traceback.print_exc()
                            error_result = {
                                **record_metadata,
                                "error": str(e),
                                "correct": False,
                                "correctness_reason": f"Exception: {e}",
                            }
                            f.write(json.dumps(error_result) + "\n")
                            f.flush()

    # Clean real-world repos after run (remove junk files written by Claude sessions)
    for repo_name in selected_repos:
        repo_path = REPOS[repo_name].path
        ensure_repo_clean(repo_path, REPOS[repo_name].commit_sha)

    # Print summary
    print()
    print("=" * 70)
    print("Benchmark complete!")
    print(f"Results saved to: {output_file}")
    print("=" * 70)
    print()
    print("To generate a report, run:")
    print(f"  python benchmark/analyze.py {output_file}")
    print()


if __name__ == "__main__":
    main()
