import subprocess
from pathlib import Path

from tasks.base import Task, GroundTruth, Mutation


class FastAPIDiffWhichCommitTask(Task):
    """3 commits, only middle one is the bug. Agent must navigate history."""

    @property
    def name(self) -> str:
        return "fastapi_diff_which_commit"

    @property
    def repo(self) -> str:
        return "fastapi"

    @property
    def task_type(self) -> str:
        return "edit"

    @property
    def mutations(self) -> list[Mutation]:
        # The actual bug mutation (applied in commit 2)
        return [
            Mutation(
                file_path="fastapi/dependencies/utils.py",
                original="if sub_dependant.use_cache and sub_dependant.cache_key in dependency_cache:",
                mutated="if sub_dependant.use_cache or sub_dependant.cache_key in dependency_cache:",
            )
        ]

    @property
    def test_command(self) -> list[str]:
        return ["/usr/local/bin/python3.10", "-m", "pytest",
                "tests/test_dependency_cache.py::test_sub_counter", "-x", "-q"]

    def apply_mutations(self, repo_path: str, commit: bool = True) -> None:
        """Override: apply 3 sequential commits — harmless, bug, harmless."""
        git_env = {
            "GIT_AUTHOR_NAME": "dev",
            "GIT_AUTHOR_EMAIL": "dev@test.com",
            "GIT_COMMITTER_NAME": "dev",
            "GIT_COMMITTER_EMAIL": "dev@test.com",
        }

        import os
        env = {**os.environ, **git_env}

        # Commit 1: harmless docstring addition
        utils_path = Path(repo_path) / "fastapi/dependencies/utils.py"
        content = utils_path.read_text()
        content = content.replace(
            "from fastapi",
            '"""Dependency resolution utilities."""\nfrom fastapi',
            1,
        )
        utils_path.write_text(content)

        subprocess.run(
            ["git", "add", "fastapi/dependencies/utils.py"],
            cwd=repo_path, check=True, capture_output=True, env=env,
        )
        subprocess.run(
            ["git", "commit", "-m", "docs: add module docstring to utils"],
            cwd=repo_path, check=True, capture_output=True, env=env,
        )

        # Commit 2: the actual bug (and → or)
        content = utils_path.read_text()
        for m in self.mutations:
            content = content.replace(m.original, m.mutated, 1)
        utils_path.write_text(content)

        subprocess.run(
            ["git", "add", "fastapi/dependencies/utils.py"],
            cwd=repo_path, check=True, capture_output=True, env=env,
        )
        subprocess.run(
            ["git", "commit", "-m", "refactor: simplify dependency cache check"],
            cwd=repo_path, check=True, capture_output=True, env=env,
        )

        # Commit 3: harmless comment addition
        content = utils_path.read_text()
        content = content.replace(
            "import dataclasses",
            "# Dependency resolution utilities\nimport dataclasses",
            1,
        )
        utils_path.write_text(content)

        subprocess.run(
            ["git", "add", "fastapi/dependencies/utils.py"],
            cwd=repo_path, check=True, capture_output=True, env=env,
        )
        subprocess.run(
            ["git", "commit", "-m", "refactor: shorten variable name"],
            cwd=repo_path, check=True, capture_output=True, env=env,
        )

    @property
    def prompt(self) -> str:
        return (
            "Tests started failing sometime in the last 3 commits to "
            "fastapi/dependencies/utils.py. Check the recent commit history "
            "to find which commit introduced the regression, then fix it."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()
