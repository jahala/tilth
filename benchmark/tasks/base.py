from abc import ABC, abstractmethod
from dataclasses import dataclass, field
import subprocess
from pathlib import Path


@dataclass
class GroundTruth:
    """Expected elements for correctness validation."""
    required_strings: list[str]
    forbidden_strings: list[str] = field(default_factory=lambda: [
        "I cannot", "I don't have access", "no such file",
    ])
    # For edit tasks only:
    file_path: str = ""
    expected_diff_contains: list[str] = field(default_factory=list)


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

    def check_correctness(self, result_text: str, repo_path: str) -> tuple[bool, str]:
        """Validate result against ground truth."""
        gt = self.ground_truth
        # Strip markdown backticks so "trait Matcher" matches "`Matcher` trait" etc.
        text_lower = result_text.replace("`", "").lower()

        for required in gt.required_strings:
            if required.lower() not in text_lower:
                return False, f"Missing: {required}"

        for forbidden in gt.forbidden_strings:
            if forbidden.lower() in text_lower:
                return False, f"Contains forbidden: {forbidden}"

        if self.task_type == "edit" and gt.file_path:
            result = subprocess.run(
                ["git", "diff", gt.file_path],
                cwd=repo_path, capture_output=True, text=True,
            )
            diff = result.stdout
            if not diff:
                return False, "No changes in target file"
            for pattern in gt.expected_diff_contains:
                if pattern not in diff:
                    return False, f"Diff missing: {pattern}"

        return True, "All checks passed"
