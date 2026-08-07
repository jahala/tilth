#!/usr/bin/env python3
"""Resolve mutable Git branches into a pinned benchmark experiment manifest."""

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

from variants import load_experiment

DEFAULT_REFERENCE_REPOSITORY = "https://github.com/jahala/tilth"
DEFAULT_ARM_ORDER_SEED = 163162
_SHA_RE = re.compile(r"[0-9a-f]{40}")
_GIT_URL_PREFIXES = ("https://", "ssh://", "git@")


def _validate_repository(repository: str) -> None:
    if not repository.startswith(_GIT_URL_PREFIXES):
        raise ValueError(f"repository must be a git URL: {repository}")


def resolve_git_ref(repository: str, git_ref: str) -> str:
    """Resolve a branch name to a commit SHA; pass exact SHAs through unchanged."""
    _validate_repository(repository)
    if _SHA_RE.fullmatch(git_ref):
        return git_ref

    branch = git_ref.removeprefix("refs/heads/")
    if not branch or branch != branch.strip() or "\0" in branch:
        raise ValueError(f"invalid branch name: {git_ref!r}")
    full_ref = f"refs/heads/{branch}"
    result = subprocess.run(
        ["git", "ls-remote", "--exit-code", repository, full_ref],
        capture_output=True,
        text=True,
        timeout=60,
    )
    matches = [
        sha
        for line in result.stdout.splitlines()
        if "\t" in line
        for sha, resolved_ref in [line.split("\t", 1)]
        if resolved_ref == full_ref and _SHA_RE.fullmatch(sha)
    ]
    if result.returncode != 0 or len(matches) != 1:
        raise ValueError(f"cannot resolve branch {branch} in {repository}")
    return matches[0]


def prepare_experiment(
    *,
    output: Path,
    reference_repository: str,
    reference_ref: str,
    variant_repository: str | None,
    variant_ref: str,
    arm_order_seed: int,
) -> dict:
    """Resolve both benchmark arms and write the strict pinned manifest."""
    candidate_repository = variant_repository or reference_repository
    reference_sha = resolve_git_ref(reference_repository, reference_ref)
    variant_sha = resolve_git_ref(candidate_repository, variant_ref)
    manifest = {
        "arm_order_seed": arm_order_seed,
        "variants": [
            {"name": "no_tilth"},
            {
                "name": "upstream",
                "repository": reference_repository,
                "git_ref": reference_ref.removeprefix("refs/heads/"),
                "git_sha": reference_sha,
            },
            {
                "name": "fork",
                "repository": candidate_repository,
                "git_ref": variant_ref.removeprefix("refs/heads/"),
                "git_sha": variant_sha,
            },
        ],
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(manifest, indent=2) + "\n")
    load_experiment(output)
    return manifest


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Resolve reference and variant branches into a pinned experiment",
    )
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--reference-repository",
        default=DEFAULT_REFERENCE_REPOSITORY,
    )
    parser.add_argument("--reference-ref", default="main")
    parser.add_argument(
        "--variant-repository",
        help="Defaults to --reference-repository",
    )
    parser.add_argument("--variant-ref", required=True)
    parser.add_argument(
        "--arm-order-seed",
        type=int,
        default=DEFAULT_ARM_ORDER_SEED,
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        manifest = prepare_experiment(
            output=args.output,
            reference_repository=args.reference_repository,
            reference_ref=args.reference_ref,
            variant_repository=args.variant_repository,
            variant_ref=args.variant_ref,
            arm_order_seed=args.arm_order_seed,
        )
    except (OSError, ValueError, subprocess.TimeoutExpired) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    reference, variant = manifest["variants"][1:]
    print(f"Manifest:  {args.output.resolve()}")
    print(f"Reference: {reference['git_ref']} -> {reference['git_sha']}")
    print(f"Variant:   {variant['git_ref']} -> {variant['git_sha']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
