#!/usr/bin/env python3
"""Build every pinned tilth variant declared by an experiment manifest."""

import argparse
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from variants import Experiment, load_experiment


def install_variants(experiment: Experiment) -> dict[str, Path]:
    """Install each tilth variant from its exact git revision."""
    installed: dict[str, Path] = {}
    for variant in experiment.variants:
        if variant.binary_path is None:
            continue
        root = variant.binary_path.parent.parent
        root.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            [
                "cargo",
                "install",
                "--git",
                variant.repository,
                "--rev",
                variant.git_sha,
                "--root",
                str(root),
                "--locked",
                "--force",
                "tilth",
            ],
            check=True,
        )
        if not variant.binary_path.is_file():
            raise RuntimeError(
                f"cargo install completed without creating {variant.binary_path}"
            )
        installed[variant.name] = variant.binary_path
    return installed


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("experiment", type=Path, help="Pinned experiment JSON manifest")
    args = parser.parse_args()

    try:
        experiment = load_experiment(args.experiment)
        installed = install_variants(experiment)
    except (ValueError, RuntimeError, OSError, subprocess.SubprocessError) as error:
        parser.exit(1, f"ERROR: {error}\n")

    for name, binary in installed.items():
        print(f"{name}: {binary}")


if __name__ == "__main__":
    main()
