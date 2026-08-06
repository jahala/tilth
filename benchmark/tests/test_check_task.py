from __future__ import annotations

import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock

sys.path.insert(0, str(Path(__file__).parent.parent))

import check_task
from tasks.base import Mutation, TaskSource


class FakeTask:
    def __init__(
        self,
        name: str = "fixture_task",
        *,
        repo: str = "synthetic",
        mutations: list[Mutation] | None = None,
        test_command: list[str] | None = None,
        apply: Mock | None = None,
        capability: str = "fix",
    ) -> None:
        self.name = name
        self.repo = repo
        self.mutations = mutations if mutations is not None else [
            Mutation("broken.py", "good", "bad")
        ]
        self.test_command = test_command if test_command is not None else ["pytest", "-q"]
        self.apply = apply or Mock()
        self.capability = capability
        self.source = TaskSource()

    def apply_mutations(self, repo_path: str) -> None:
        self.apply(repo_path)


def _completed(command: list[str], returncode: int, stdout: str = "", stderr: str = ""):
    return subprocess.CompletedProcess(command, returncode, stdout=stdout, stderr=stderr)


def _install_task(monkeypatch, task: FakeTask) -> None:
    monkeypatch.setattr(check_task, "TASKS", {task.name: task})


def test_mutation_task_requires_pass_before_and_fail_after_then_cleans_up(
    monkeypatch, tmp_path, capsys
):
    repo_path = tmp_path / "repo"
    repo_path.mkdir()
    task = FakeTask()
    _install_task(monkeypatch, task)
    events: list[object] = []
    task.apply.side_effect = lambda path: events.append(("apply", path))
    monkeypatch.setattr(check_task, "get_repo_path", lambda repo: events.append(("path", repo)) or repo_path)
    monkeypatch.setattr(check_task, "reset_repo", lambda: events.append("reset"))

    outcomes = iter([_completed(task.test_command, 0), _completed(task.test_command, 1)])

    def run(command, *, cwd, capture_output, text):
        events.append(("run", command, cwd, capture_output, text))
        return next(outcomes)

    monkeypatch.setattr(check_task.subprocess, "run", run)
    assert check_task.main([task.name]) == 0
    assert events == [
        ("path", "synthetic"),
        "reset",
        ("run", task.test_command, repo_path, True, True),
        ("apply", str(repo_path)),
        ("run", task.test_command, repo_path, True, True),
        "reset",
    ]
    task.apply.assert_called_once_with(str(repo_path))
    assert "PASS fixture_task: baseline passed; mutation failed" in capsys.readouterr().out

def test_mutation_task_requires_nonempty_test_command(monkeypatch, tmp_path, capsys):
    repo_path = tmp_path / "repo"
    repo_path.mkdir()
    task = FakeTask(test_command=[])
    _install_task(monkeypatch, task)
    events: list[str] = []
    monkeypatch.setattr(check_task, "get_repo_path", lambda repo: repo_path)
    monkeypatch.setattr(check_task, "reset_repo", lambda: events.append("reset"))
    run = Mock()
    monkeypatch.setattr(check_task.subprocess, "run", run)

    assert check_task.main([task.name]) == 1
    assert events == ["reset", "reset"]
    run.assert_not_called()
    task.apply.assert_not_called()
    assert capsys.readouterr().out == (
        "FAIL fixture_task: mutation task requires a nonempty test_command\n"
    )



def test_preexisting_failure_rejects_without_applying_mutations(monkeypatch, tmp_path, capsys):
    repo_path = tmp_path / "repo"
    repo_path.mkdir()
    task = FakeTask()
    _install_task(monkeypatch, task)
    events: list[str] = []
    monkeypatch.setattr(check_task, "get_repo_path", lambda repo: repo_path)
    monkeypatch.setattr(check_task, "reset_repo", lambda: events.append("reset"))
    monkeypatch.setattr(
        check_task.subprocess,
        "run",
        lambda *args, **kwargs: (events.append("run") or _completed(task.test_command, 1, "before out\n", "before err\n")),
    )

    assert check_task.main([task.name]) == 1
    assert events == ["reset", "run", "reset"]
    task.apply.assert_not_called()
    captured = capsys.readouterr()
    assert "FAIL fixture_task: pre-mutation test failed (exit code 1)" in captured.out
    assert "before out" in captured.out
    assert "before err" in captured.err


def test_mutation_that_does_not_break_tests_is_rejected(monkeypatch, tmp_path, capsys):
    repo_path = tmp_path / "repo"
    repo_path.mkdir()
    task = FakeTask()
    _install_task(monkeypatch, task)
    events: list[str] = []
    monkeypatch.setattr(check_task, "get_repo_path", lambda repo: repo_path)
    monkeypatch.setattr(check_task, "reset_repo", lambda: events.append("reset"))
    outcomes = iter([_completed(task.test_command, 0), _completed(task.test_command, 0)])
    monkeypatch.setattr(
        check_task.subprocess,
        "run",
        lambda *args, **kwargs: (events.append("run") or next(outcomes)),
    )

    assert check_task.main([task.name]) == 1
    assert events == ["reset", "run", "run", "reset"]
    task.apply.assert_called_once_with(str(repo_path))
    assert "FAIL fixture_task: mutation did not make test_command fail" in capsys.readouterr().out


def test_unknown_task_is_rejected_before_repo_or_subprocess_work(monkeypatch, capsys):
    monkeypatch.setattr(check_task, "TASKS", {})
    reset = Mock()
    run = Mock()
    monkeypatch.setattr(check_task, "reset_repo", reset)
    monkeypatch.setattr(check_task.subprocess, "run", run)

    assert check_task.main(["missing_task"]) == 2
    assert capsys.readouterr().err == "Unknown task: missing_task\n"
    reset.assert_not_called()
    run.assert_not_called()


def test_cleanup_runs_when_mutation_application_raises(monkeypatch, tmp_path, capsys):
    repo_path = tmp_path / "repo"
    repo_path.mkdir()
    apply = Mock(side_effect=RuntimeError("mutation exploded"))
    task = FakeTask(apply=apply)
    _install_task(monkeypatch, task)
    events: list[str] = []
    monkeypatch.setattr(check_task, "get_repo_path", lambda repo: repo_path)
    monkeypatch.setattr(check_task, "reset_repo", lambda: events.append("reset"))
    monkeypatch.setattr(
        check_task.subprocess,
        "run",
        lambda *args, **kwargs: (events.append("run") or _completed(task.test_command, 0)),
    )

    assert check_task.main([task.name]) == 1
    assert events == ["reset", "run", "reset"]
    assert "FAIL fixture_task: mutation application failed: mutation exploded" in capsys.readouterr().out


def test_cleanup_runs_when_subprocess_raises_and_preserves_exception_diagnostics(
    monkeypatch, tmp_path, capsys
):
    repo_path = tmp_path / "repo"
    repo_path.mkdir()
    task = FakeTask()
    _install_task(monkeypatch, task)
    events: list[str] = []
    monkeypatch.setattr(check_task, "get_repo_path", lambda repo: repo_path)
    monkeypatch.setattr(check_task, "reset_repo", lambda: events.append("reset"))
    error = subprocess.CalledProcessError(
        7,
        task.test_command,
        output="raised out\n",
        stderr="raised err\n",
    )
    monkeypatch.setattr(
        check_task.subprocess,
        "run",
        lambda *args, **kwargs: (events.append("run") or (_ for _ in ()).throw(error)),
    )

    assert check_task.main([task.name]) == 1
    assert events == ["reset", "run", "reset"]
    captured = capsys.readouterr()
    assert "FAIL fixture_task: pre-mutation test command raised CalledProcessError" in captured.out
    assert "raised out" in captured.out
    assert "raised err" in captured.err


def test_cleanup_failure_is_reported_after_otherwise_valid_preflight(monkeypatch, tmp_path, capsys):
    repo_path = tmp_path / "repo"
    repo_path.mkdir()
    task = FakeTask()
    _install_task(monkeypatch, task)
    monkeypatch.setattr(check_task, "get_repo_path", lambda repo: repo_path)
    reset = Mock(side_effect=[None, RuntimeError("cleanup exploded")])
    monkeypatch.setattr(check_task, "reset_repo", reset)
    outcomes = iter([_completed(task.test_command, 0), _completed(task.test_command, 1)])
    monkeypatch.setattr(check_task.subprocess, "run", lambda *args, **kwargs: next(outcomes))

    assert check_task.main([task.name]) == 1
    assert reset.call_count == 2
    assert capsys.readouterr().out == (
        "FAIL fixture_task: cleanup failed: RuntimeError: cleanup exploded\n"
    )


def test_metadata_validator_rejects_invalid_capability_and_provenance():
    task = FakeTask(capability="unknown")

    assert check_task.validate_task_metadata(task) == (
        "invalid capability 'unknown'; expected one of control, debug, fix, locate, trace"
    )

    task.capability = "fix"
    task.source = TaskSource(origin="")
    assert check_task.validate_task_metadata(task) == "source.origin must be a nonempty string"

    task.repo = "clone"
    task.source = TaskSource()
    assert (
        check_task.validate_task_metadata(task)
        == "external task source must identify its pinned repository"
    )

def test_cloned_repo_resets_to_configured_revision(monkeypatch, tmp_path):
    repo_path = tmp_path / "clone"
    repo_path.mkdir()
    task = FakeTask(repo="clone")
    task.source = TaskSource(origin="clone", commit_or_tag="pinned-revision")
    _install_task(monkeypatch, task)
    events: list[object] = []
    monkeypatch.setattr(check_task, "get_repo_path", lambda repo: events.append(("path", repo)) or repo_path)
    monkeypatch.setattr(
        check_task,
        "REPOS",
        {"clone": SimpleNamespace(commit_sha="pinned-revision")},
    )
    monkeypatch.setattr(
        check_task,
        "ensure_repo_clean",
        lambda path, revision: events.append(("clean", path, revision)),
    )
    outcomes = iter([_completed(task.test_command, 0), _completed(task.test_command, 1)])
    monkeypatch.setattr(check_task.subprocess, "run", lambda *args, **kwargs: next(outcomes))

    assert check_task.main([task.name]) == 0
    assert events == [
        ("path", "clone"),
        ("clean", repo_path, "pinned-revision"),
        ("clean", repo_path, "pinned-revision"),
    ]


def test_nonmutation_task_reports_no_preflight_and_succeeds(monkeypatch, capsys):
    task = FakeTask(mutations=[], test_command=[])
    _install_task(monkeypatch, task)
    reset = Mock()
    monkeypatch.setattr(check_task, "reset_repo", reset)

    assert check_task.main([task.name]) == 0
    assert capsys.readouterr().out == "PASS fixture_task: no mutation preflight needed\n"
    reset.assert_not_called()