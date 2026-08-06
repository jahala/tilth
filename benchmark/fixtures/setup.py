#!/usr/bin/env python3
"""
Materializes a deterministic Flask-based web application with specific
ground truth strings embedded for correctness checking.
"""

import shutil
import subprocess
from pathlib import Path

FIXTURE_PATH = Path(__file__).parent
TEMPLATE_PATH = FIXTURE_PATH / "template"
REPO_PATH = FIXTURE_PATH / "repo"


def setup_repo():
    """Materialize the synthetic repository from tracked fixture files."""
    if REPO_PATH.exists():
        print(f"Removing existing repo at {REPO_PATH}")
        shutil.rmtree(REPO_PATH)

    print(f"Creating repo at {REPO_PATH}")
    shutil.copytree(
        TEMPLATE_PATH,
        REPO_PATH,
        ignore=shutil.ignore_patterns("__pycache__", "*.pyc", "*.pyo"),
    )

    file_stats = []
    for path in sorted(path for path in REPO_PATH.rglob("*") if path.is_file()):
        relative_path = path.relative_to(REPO_PATH).as_posix()
        line_count = len(path.read_text(encoding="utf-8").splitlines())
        file_stats.append((relative_path, line_count))
        print(f"  Created {relative_path} ({line_count} lines)")

    print("\\nInitializing git repository...")
    subprocess.run(
        ["git", "init"],
        cwd=REPO_PATH,
        check=True,
        capture_output=True,
    )
    subprocess.run(
        ["git", "add", "."],
        cwd=REPO_PATH,
        check=True,
        capture_output=True,
    )
    subprocess.run(
        ["git", "commit", "-m", "Initial commit"],
        cwd=REPO_PATH,
        check=True,
        capture_output=True,
    )

    print("\\n" + "=" * 60)
    print("Repository setup complete!")
    print("=" * 60)
    print(f"\\nLocation: {REPO_PATH}")
    print(f"Total files: {len(file_stats)}")
    print(f"Total lines: {sum(count for _, count in file_stats)}")

    print("\\nFile breakdown:")
    for file_path, line_count in sorted(file_stats, key=lambda item: -item[1]):
        print(f"  {file_path:40s} {line_count:4d} lines")


if __name__ == "__main__":
    setup_repo()
