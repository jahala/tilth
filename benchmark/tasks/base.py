import os
import shlex
import subprocess
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from functools import cached_property
from pathlib import Path

from config import REPOS


@dataclass
class Mutation:
    """A single file mutation: replace `original` with `mutated` to introduce a bug."""
    file_path: str
    original: str
    mutated: str


@dataclass
class GroundTruth:
    """Expected elements for correctness validation."""
    # Each entry is AND-matched; use "a|b" within an entry for OR alternation ("|" is reserved).
    required_strings: list[str] = field(default_factory=list)
    forbidden_strings: list[str] = field(default_factory=lambda: [
        "I cannot", "I don't have access", "no such file",
    ])
    # For forward-edit tasks only (no mutations):
    file_path: str = ""
    expected_diff_contains: list[str] = field(default_factory=list)


@dataclass(frozen=True)
class TaskSource:
    origin: str = "original"
    license: str = "MIT"
    commit_or_tag: str = "fixture-pin"
    transformation: str = "none"


DEFAULT_TASK_SOURCE = TaskSource()


def required_matches(required: str, text_lower: str) -> bool:
    """True if `required` is satisfied in `text_lower`. A "|" makes the entry an
    alternation: OR within the entry, so "foo|bar" matches when either substring
    is present. An entry with no "|" is a plain substring match (backward-compatible).

    Alternates are whitespace-stripped, so "foo | bar" behaves like "foo|bar". Empty
    alternates are ignored, so a leading/trailing/doubled "|" cannot turn the entry
    into an unconditional pass; an all-empty required string never matches. Literal
    "|" is therefore reserved in required_strings.
    """
    alternates = [alt for alt in (a.strip() for a in required.split("|")) if alt]
    return any(alt.lower() in text_lower for alt in alternates)


class Task(ABC):
    @property
    @abstractmethod
    def name(self) -> str: ...

    @property
    @abstractmethod
    def prompt(self) -> str: ...

    @property
    @abstractmethod
    def ground_truth(self) -> GroundTruth: ...

    @property
    def task_type(self) -> str:
        return "read"

    @property
    def repo(self) -> str:
        """Repository this task targets. Default: synthetic."""
        return "synthetic"

    @property
    def capability(self) -> str:
        return ""

    @cached_property
    def source(self) -> TaskSource:
        if self.repo == "synthetic":
            return DEFAULT_TASK_SOURCE
        repo = REPOS[self.repo]
        return TaskSource(
            origin=repo.url,
            license=repo.license,
            commit_or_tag=repo.commit_sha,
        )

    @property
    def mutations(self) -> list[Mutation]:
        """Mutations to apply before the agent runs. Empty for non-mutation tasks."""
        return []

    @property
    def test_command(self) -> list[str]:
        """Command to validate the fix. Empty = no test-based validation."""
        return []

    @property
    def hide_git(self) -> bool:
        """Hide repository history for fix tasks unless the task opts out."""
        return self.capability == "fix" and bool(self.mutations)

    def apply_mutations(self, repo_path: str) -> None:
        """Apply configured mutations to the task workspace."""
        for m in self.mutations:
            fp = Path(repo_path) / m.file_path
            content = fp.read_text()
            if m.original not in content:
                raise ValueError(
                    f"Mutation target not found in {m.file_path}: "
                    f"{m.original[:80]!r}"
                )
            content = content.replace(m.original, m.mutated, 1)
            fp.write_text(content)

        if self.hide_git:
            return

        mutated_files = [m.file_path for m in self.mutations]
        git_env = {
            "GIT_AUTHOR_NAME": "dev",
            "GIT_AUTHOR_EMAIL": "dev@test.com",
            "GIT_COMMITTER_NAME": "dev",
            "GIT_COMMITTER_EMAIL": "dev@test.com",
        }
        env = {**os.environ, **git_env}
        subprocess.run(
            ["git", "add"] + mutated_files,
            cwd=repo_path, check=True, capture_output=True, env=env,
        )
        subprocess.run(
            ["git", "commit", "-m", "refactor: simplify edge case handling"],
            cwd=repo_path, check=True, capture_output=True, env=env,
        )

    def check_correctness(self, result_text: str, repo_path: str) -> tuple[bool, str]:
        """Validate result against ground truth."""
        gt = self.ground_truth


        # Mutation tasks with a test command: run the test. That's the source of truth.
        if self.task_type == "edit" and self.mutations and self.test_command:
            result = subprocess.run(
                self.test_command,
                cwd=repo_path, capture_output=True, text=True,
                timeout=300,
            )
            if result.returncode != 0:
                return False, f"Test failed: {shlex.join(self.test_command)}"
            return True, "Test passed"

        # Forward-edit tasks: check git diff for expected patterns.
        diff = ""
        if self.task_type == "edit" and gt.file_path:
            result = subprocess.run(
                ["git", "diff", gt.file_path],
                cwd=repo_path, capture_output=True, text=True,
            )
            diff = result.stdout
            if not self.mutations:
                if not diff:
                    return False, "No changes in target file"
                for pattern in gt.expected_diff_contains:
                    if pattern not in diff:
                        return False, f"Diff missing: {pattern}"

        # Read tasks / forward-edit tasks: check required_strings in response + diff.
        combined = (result_text + "\n" + diff).replace("`", "")
        text_lower = combined.lower()

        for required in gt.required_strings:
            if not required_matches(required, text_lower):
                return False, f"Missing: {required}"

        for forbidden in gt.forbidden_strings:
            if forbidden.lower() in text_lower:
                return False, f"Contains forbidden: {forbidden}"

        return True, "All checks passed"
