#!/usr/bin/env python3
"""Verify that benchmark mutation tasks break their validation tests."""

from __future__ import annotations

import argparse
import subprocess
import sys
from collections.abc import Sequence
from pathlib import Path
from typing import Any

# Keep this module runnable as ``python benchmark/check_task.py`` as well as
# importable by the benchmark tests.
sys.path.insert(0, str(Path(__file__).parent))

from config import REPOS
from fixtures.reset import ensure_repo_clean, reset_repo, restore_git
from run import get_repo_path
from tasks import TASKS


CAPABILITIES = frozenset({"locate", "trace", "fix", "debug", "control"})
_SOURCE_FIELDS = ("origin", "license", "commit_or_tag", "transformation")


def validate_task_metadata(task: Any) -> str | None:
    """Return a diagnostic when a task's capability or provenance is invalid."""
    capability = getattr(task, "capability", None)
    if capability not in CAPABILITIES:
        return (
            f"invalid capability {capability!r}; expected one of "
            f"{', '.join(sorted(CAPABILITIES))}"
        )

    source = getattr(task, "source", None)
    if source is None:
        return "missing or invalid source metadata"
    for field in _SOURCE_FIELDS:
        value = getattr(source, field, None)
        if not isinstance(value, str) or not value.strip():
            return f"source.{field} must be a nonempty string"
    if (
        getattr(task, "repo", "synthetic") != "synthetic"
        and source.origin == "original"
        and source.commit_or_tag == "fixture-pin"
    ):
        return "external task source must identify its pinned repository"
    return None


def _reset_task_repo(task: Any, repo_path: Path) -> None:
    """Restore a task repository to its configured baseline."""
    if task.repo == "synthetic":
        restore_git(repo_path)
        reset_repo()
        return

    try:
        revision = REPOS[task.repo].commit_sha
    except KeyError as exc:
        raise ValueError(f"no configured revision for repository {task.repo!r}") from exc
    ensure_repo_clean(repo_path, revision)


def _text(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode(errors="replace")
    return str(value)


def _write_diagnostic(task_name: str, phase: str, stream_name: str, value: Any, stream) -> None:
    text = _text(value)
    if not text:
        return
    stream.write(f"{task_name} {phase} {stream_name}:\n{text}")
    if not text.endswith("\n"):
        stream.write("\n")


def _write_process_diagnostics(task_name: str, phase: str, result: Any) -> None:
    _write_diagnostic(task_name, phase, "stdout", getattr(result, "stdout", ""), sys.stdout)
    _write_diagnostic(task_name, phase, "stderr", getattr(result, "stderr", ""), sys.stderr)


def _write_exception_diagnostics(task_name: str, phase: str, error: BaseException) -> None:
    output = getattr(error, "stdout", None)
    if output is None:
        output = getattr(error, "output", None)
    _write_diagnostic(task_name, phase, "stdout", output, sys.stdout)
    _write_diagnostic(task_name, phase, "stderr", getattr(error, "stderr", ""), sys.stderr)


def _run_test(command: list[str], repo_path: Path) -> tuple[Any | None, BaseException | None]:
    try:
        result = subprocess.run(command, cwd=repo_path, capture_output=True, text=True)
    except Exception as exc:  # The caller reports the phase and still cleans up.
        return None, exc
    return result, None


def check_task(task_name: str) -> bool:
    """Run the mutation preflight for one registered task and print its result."""
    task = TASKS[task_name]
    metadata_error = validate_task_metadata(task)
    if metadata_error:
        print(f"FAIL {task_name}: {metadata_error}")
        return False

    mutations = task.mutations
    if not mutations:
        print(f"PASS {task_name}: no mutation preflight needed")
        return True

    command = task.test_command
    repo_path = get_repo_path(task.repo)
    reason: str | None = None
    diagnostic: tuple[str, Any] | None = None

    try:
        _reset_task_repo(task, repo_path)
        if not command:
            reason = "mutation task requires a nonempty test_command"
        else:
            before, before_error = _run_test(command, repo_path)
            if before_error is not None:
                reason = (
                    "pre-mutation test command raised "
                    f"{type(before_error).__name__}"
                )
                if str(before_error):
                    reason += f": {before_error}"
                diagnostic = ("pre-mutation", before_error)
            elif before.returncode != 0:
                reason = f"pre-mutation test failed (exit code {before.returncode})"
                diagnostic = ("pre-mutation", before)
            else:
                try:
                    task.apply_mutations(str(repo_path))
                except Exception as exc:
                    reason = f"mutation application failed: {exc}"
                else:
                    after, after_error = _run_test(command, repo_path)
                    if after_error is not None:
                        reason = (
                            "post-mutation test command raised "
                            f"{type(after_error).__name__}"
                        )
                        if str(after_error):
                            reason += f": {after_error}"
                        diagnostic = ("post-mutation", after_error)
                    elif after.returncode == 0:
                        reason = "mutation did not make test_command fail"
                    else:
                        reason = None
    except Exception as exc:
        reason = f"preflight raised {type(exc).__name__}: {exc}"
    finally:
        try:
            _reset_task_repo(task, repo_path)
        except Exception as exc:
            cleanup_reason = f"cleanup failed: {type(exc).__name__}: {exc}"
            reason = cleanup_reason if reason is None else f"{reason}; {cleanup_reason}"

    if reason is None:
        print(f"PASS {task_name}: baseline passed; mutation failed")
        return True

    print(f"FAIL {task_name}: {reason}")
    if diagnostic is not None:
        phase, value = diagnostic
        if isinstance(value, BaseException):
            _write_exception_diagnostics(task_name, phase, value)
        else:
            _write_process_diagnostics(task_name, phase, value)
    return False


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Check benchmark mutation tasks")
    parser.add_argument("task", nargs="?", help="registered task name (default: all tasks)")
    args = parser.parse_args(argv)

    if args.task is None:
        task_names = list(TASKS)
    elif args.task not in TASKS:
        print(f"Unknown task: {args.task}", file=sys.stderr)
        return 2
    else:
        task_names = [args.task]

    metadata_failures = []
    for name in task_names:
        error = validate_task_metadata(TASKS[name])
        if error is not None:
            metadata_failures.append((name, error))
    if metadata_failures:
        for name, error in metadata_failures:
            print(f"FAIL {name}: {error}")
        return 1

    failures = 0
    for name in task_names:
        if not check_task(name):
            failures += 1
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
