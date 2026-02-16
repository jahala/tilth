#!/usr/bin/env python3
"""
Benchmark runner for tilth performance evaluation.

Executes `claude -p` for each combination of (task, mode, model, repetition).
Records token usage, cost, correctness, and tool usage to JSONL format.
"""

import argparse
import json
import os
import subprocess
import sys
import time
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
)
from parse import parse_stream_json, parse_codex_json, tool_call_counts
from tasks import TASKS
from fixtures.reset import reset_repo, ensure_repo_clean


def _tilth_version() -> Optional[str]:
    """Get installed tilth version via `tilth --version`."""
    try:
        result = subprocess.run(
            ["/Users/flysikring/.cargo/bin/tilth", "--version"],
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
                # skip other large args
            if args:
                entry["args"] = args
            seq.append(entry)
    return seq


def run_single(
    task_name: str,
    mode_name: str,
    model_name: str,
    repetition: int,
    verbose: bool = False,
) -> dict:
    """
    Run a single benchmark iteration.

    Args:
        task_name: Name of task to run
        mode_name: Mode (baseline or tilth)
        model_name: Model (haiku, sonnet, opus)
        repetition: Repetition number
        verbose: Whether to print detailed output

    Returns:
        Dictionary with benchmark results
    """
    task = TASKS[task_name]
    repo_path = get_repo_path(task.repo)
    mode = MODES[mode_name]
    model_id = MODELS[model_name]
    runner = RUNNERS[model_name]

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
            "--system-prompt", SYSTEM_PROMPT,
        ]

        if mode.tools:
            cmd += ["--tools", ",".join(mode.tools)]

        if mode.mcp_config_path:
            cmd += ["--mcp-config", mode.mcp_config_path]

        cmd += ["--", task.prompt]

    if verbose:
        print(f"    Running: {' '.join(cmd)}")

    # Run subprocess (unset CLAUDECODE to allow nested claude -p)
    env = {k: v for k, v in os.environ.items() if k != "CLAUDECODE"}
    start_time = time.time()
    result = subprocess.run(
        cmd,
        cwd=str(repo_path),
        capture_output=True,
        text=True,
        timeout=300,
        env=env,
    )
    elapsed_ms = int((time.time() - start_time) * 1000)

    if result.returncode != 0:
        runner_name = "codex exec" if runner == "codex" else "claude -p"
        raise RuntimeError(
            f"{runner_name} failed with code {result.returncode}\n"
            f"stderr: {result.stderr}\n"
            f"stdout: {result.stdout[:500]}"
        )

    # Parse output based on runner
    if runner == "codex":
        run_result = parse_codex_json(result.stdout, model_id)
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

    # Collect per-turn context tokens (input + cache = actual context processed)
    per_turn_context = [turn.context_tokens for turn in run_result.turns]
    total_context = sum(per_turn_context)

    # Return JSON-serializable dict
    return {
        "task": task_name,
        "repo": task.repo,
        "mode": mode_name,
        "model": model_name,
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
        "cache_read_tokens": run_result.total_cache_read_tokens,
        "per_turn_context_tokens": per_turn_context,
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

    args = parser.parse_args()

    # Parse and validate inputs
    try:
        models = parse_comma_list(args.models, MODELS, "models")
        tasks_list = parse_comma_list(args.tasks, TASKS, "tasks")
        modes = parse_comma_list(args.modes, MODES, "modes")
    except ValueError as e:
        parser.error(str(e))
        return

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
        ensure_repo_clean(repo_path)
        if args.verbose:
            print(f"Cleaned repo: {repo_name}")

    # Create results directory
    RESULTS_DIR.mkdir(exist_ok=True)

    # Create timestamped output file (include model name to avoid collisions
    # when multiple benchmark processes run in parallel)
    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    model_suffix = f"_{models[0]}" if len(models) == 1 else ""
    output_file = RESULTS_DIR / f"benchmark_{timestamp}{model_suffix}.jsonl"

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

                        # Reset repo if needed (only for synthetic edit tasks)
                        needs_reset = False
                        if task.repo == "synthetic":
                            if task.task_type == "edit":
                                if rep > 0 or mode_name != prev_mode or task_name != prev_task:
                                    needs_reset = True
                            elif mode_name != prev_mode:
                                needs_reset = True

                        if needs_reset:
                            if args.verbose:
                                print(f"  Resetting repo...")
                            reset_repo()

                        prev_task = task_name
                        prev_mode = mode_name

                        # Print progress
                        print(f"[{current_run}/{total_runs}] {run_id}")

                        # Run benchmark
                        try:
                            result = run_single(
                                task_name,
                                mode_name,
                                model_name,
                                rep,
                                verbose=args.verbose,
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
                            print(f"  ✗ TIMEOUT (>300s)")
                            error_result = {
                                "task": task_name,
                                "mode": mode_name,
                                "model": model_name,
                                "repetition": rep,
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
                                "task": task_name,
                                "mode": mode_name,
                                "model": model_name,
                                "repetition": rep,
                                "error": str(e),
                                "correct": False,
                                "correctness_reason": f"Exception: {e}",
                            }
                            f.write(json.dumps(error_result) + "\n")
                            f.flush()

    # Clean real-world repos after run (remove junk files written by Claude sessions)
    for repo_name in selected_repos:
        repo_path = REPOS[repo_name].path
        ensure_repo_clean(repo_path)

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
