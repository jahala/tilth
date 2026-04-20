from __future__ import annotations

import subprocess
from pathlib import Path

REPO_PATH = Path(__file__).parent / "repo"

def reset_repo():
    subprocess.run(["git", "checkout", "--", "."], cwd=REPO_PATH, check=True, capture_output=True)
    subprocess.run(["git", "clean", "-fd"], cwd=REPO_PATH, check=True, capture_output=True)

def ensure_repo_clean(repo_path: Path, pinned_sha: str | None = None) -> None:
    """Reset a real-world repo to its pinned commit.

    Handles both uncommitted changes (dirty working tree) and committed
    mutations (extra commits on top of the pinned SHA).
    """
    needs_reset = False

    # Check for dirty working tree
    status = subprocess.run(
        ["git", "status", "--porcelain"],
        cwd=str(repo_path), capture_output=True, text=True,
    )
    if status.stdout.strip():
        needs_reset = True

    # Check if HEAD matches pinned SHA
    if pinned_sha:
        head = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=str(repo_path), capture_output=True, text=True,
        )
        if head.stdout.strip() != pinned_sha:
            needs_reset = True

    if needs_reset:
        target = pinned_sha or "HEAD"
        subprocess.run(
            ["git", "checkout", "--force", target],
            cwd=str(repo_path), check=True, capture_output=True,
        )
        subprocess.run(
            ["git", "clean", "-fd"],
            cwd=str(repo_path), check=True, capture_output=True,
        )

if __name__ == "__main__":
    reset_repo()
    print(f"Reset {REPO_PATH}")
