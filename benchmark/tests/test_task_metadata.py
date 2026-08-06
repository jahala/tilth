import shlex
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent))

from tasks.base import GroundTruth, Mutation, Task, TaskSource


class FixTask(Task):
    name = "fix"
    repo = "synthetic"
    capability = "fix"

    @property
    def prompt(self) -> str:
        return "Fix it"

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()

    @property
    def mutations(self) -> list[Mutation]:
        return [Mutation("broken.py", "before", "after")]

    def check_correctness(self, result: str, repo_path: str) -> tuple[bool, str]:
        return True, "ok"


class TraceTask(FixTask):
    capability = "trace"


class FailingCommandTask(Task):
    name = "failing-command"
    task_type = "edit"
    capability = "fix"

    @property
    def prompt(self) -> str:
        return "Fix it"

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()

    @property
    def mutations(self) -> list[Mutation]:
        return [Mutation("broken.py", "before", "after")]

    @property
    def test_command(self) -> list[str]:
        return [sys.executable, "-c", "raise SystemExit(3)"]


def test_task_source_defaults_are_complete() -> None:
    assert FixTask().source == TaskSource(
        origin="original",
        license="MIT",
        commit_or_tag="fixture-pin",
        transformation="none",
    )


def test_fix_mutation_tasks_hide_git_by_default() -> None:
    assert FixTask().hide_git is True


def test_applying_fix_mutation_leaves_no_git_backup_in_repo(tmp_path) -> None:
    (tmp_path / ".git").mkdir()
    target = tmp_path / "broken.py"
    target.write_text("before")

    FixTask().apply_mutations(str(tmp_path))

    assert target.read_text() == "after"
    assert (tmp_path / ".git").is_dir()
    assert not (tmp_path / ".git_hidden").exists()


def test_non_fix_tasks_keep_git_visible() -> None:
    assert TraceTask().hide_git is False



def test_failed_task_reports_full_test_command(tmp_path) -> None:
    task = FailingCommandTask()

    correct, message = task.check_correctness("", str(tmp_path))

    assert correct is False
    assert message == f"Test failed: {shlex.join(task.test_command)}"