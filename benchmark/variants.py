"""Pinned benchmark variants and deterministic arm ordering."""

import hashlib
import json
import os
import random
import re
import subprocess
import tempfile
from dataclasses import dataclass, replace
from pathlib import Path

from config import ModeConfig

_SHA_RE = re.compile(r"[0-9a-f]{40}")
_NAME_RE = re.compile(r"[a-z][a-z0-9_-]*")
_EXPERIMENT_KEYS = {"arm_order_seed", "variants"}
_VARIANT_KEYS = {"name", "repository", "git_sha"}
_TOOLS = ["Read", "Edit", "Grep", "Glob", "Bash"]


@dataclass(frozen=True)
class Variant:
    name: str
    repository: str | None
    git_sha: str | None
    binary_path: Path | None


@dataclass(frozen=True)
class Experiment:
    path: Path
    arm_order_seed: int
    variants: tuple[Variant, ...]


def _variant_root() -> Path:
    configured = os.environ.get("TILTH_BENCH_VARIANT_ROOT")
    if configured:
        return Path(configured)
    return Path(tempfile.gettempdir()) / "tilth-benchmark" / "variants"


def _unknown_keys(data: dict, allowed: set[str], context: str) -> None:
    unknown = sorted(set(data) - allowed)
    if unknown:
        raise ValueError(f"{context} has unknown keys: {', '.join(unknown)}")


def load_experiment(
    path: Path,
    *,
    variant_root: Path | None = None,
) -> Experiment:
    """Load and validate a pinned three-way benchmark experiment."""
    try:
        data = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read experiment manifest {path}: {error}") from error
    if not isinstance(data, dict):
        raise ValueError("experiment manifest must be a JSON object")
    _unknown_keys(data, _EXPERIMENT_KEYS, "experiment manifest")

    seed = data.get("arm_order_seed")
    entries = data.get("variants")
    if not isinstance(seed, int) or isinstance(seed, bool):
        raise ValueError("arm_order_seed must be an integer")
    if not isinstance(entries, list) or not entries:
        raise ValueError("variants must be a non-empty array")

    root = (variant_root or _variant_root()).expanduser().resolve()
    variants: list[Variant] = []
    names: set[str] = set()
    for index, entry in enumerate(entries):
        if not isinstance(entry, dict):
            raise ValueError(f"variant {index} must be a JSON object")
        _unknown_keys(entry, _VARIANT_KEYS, f"variant {index}")
        name = entry.get("name")
        if not isinstance(name, str) or not _NAME_RE.fullmatch(name):
            raise ValueError(f"variant {index} name must be lowercase snake/kebab case")
        if name in names:
            raise ValueError(f"duplicate variant name: {name}")
        names.add(name)

        repository = entry.get("repository")
        git_sha = entry.get("git_sha")
        if name == "no_tilth":
            if repository is not None or git_sha is not None:
                raise ValueError("no_tilth must not specify repository or git_sha")
            binary_path = None
        else:
            if not isinstance(repository, str) or not repository.startswith(
                ("https://", "ssh://", "git@")
            ):
                raise ValueError(f"variant {name} repository must be a git URL")
            if not isinstance(git_sha, str) or not _SHA_RE.fullmatch(git_sha):
                raise ValueError(
                    f"variant {name} git_sha must be a lowercase 40-character SHA"
                )
            binary_path = (root / f"{name}-{git_sha}" / "bin" / "tilth").resolve()
        variants.append(Variant(name, repository, git_sha, binary_path))

    if names != {"no_tilth", "upstream", "fork"}:
        raise ValueError("variants must be exactly: no_tilth, upstream, fork")
    return Experiment(path.resolve(), seed, tuple(variants))


def _write_json(path: Path, value: dict) -> str:
    path.write_text(json.dumps(value, indent=2) + "\n")
    return str(path.resolve())


def experiment_modes(experiment: Experiment, config_dir: Path) -> dict[str, ModeConfig]:
    """Write per-variant runner configs and return matching mode definitions."""
    config_dir.mkdir(parents=True, exist_ok=True)
    modes: dict[str, ModeConfig] = {}
    for variant in experiment.variants:
        opencode_path = config_dir / f"opencode-{variant.name}.json"
        if variant.binary_path is None:
            mcp_path = None
            _write_json(
                opencode_path,
                {"$schema": "https://opencode.ai/config.json", "mcp": {}},
            )
        else:
            binary = str(variant.binary_path)
            mcp_path = _write_json(
                config_dir / f"claude-{variant.name}.json",
                {
                    "mcpServers": {
                        "tilth": {
                            "type": "stdio",
                            "command": binary,
                            "args": ["--mcp", "--edit"],
                        }
                    }
                },
            )
            _write_json(
                opencode_path,
                {
                    "$schema": "https://opencode.ai/config.json",
                    "mcp": {
                        "tilth": {
                            "type": "local",
                            "enabled": True,
                            "command": [binary, "--mcp", "--edit"],
                        }
                    },
                },
            )
        modes[variant.name] = ModeConfig(
            name=variant.name,
            tools=list(_TOOLS),
            mcp_config_path=mcp_path,
            opencode_config_path=str(opencode_path.resolve()),
            binary_path=str(variant.binary_path) if variant.binary_path else None,
            repository=variant.repository,
            git_sha=variant.git_sha,
            description=(
                "Built-in tools without tilth"
                if variant.binary_path is None
                else f"Built-in tools + pinned {variant.name} tilth MCP"
            ),
        )
    return modes


def randomized_arm_order(
    arms: list[str],
    *,
    seed: int,
    task: str,
    model: str,
    repetition: int,
) -> list[str]:
    """Return a stable pseudorandom arm order for one matched run block."""
    material = f"{seed}\0{task}\0{model}\0{repetition}".encode()
    block_seed = int.from_bytes(hashlib.sha256(material).digest(), "big")
    ordered = list(arms)
    random.Random(block_seed).shuffle(ordered)
    return ordered


def _command_version(command: list[str]) -> str:
    try:
        result = subprocess.run(
            command,
            check=True,
            capture_output=True,
            text=True,
            timeout=10,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise ValueError(f"cannot run {' '.join(command)}: {error}") from error
    return result.stdout.strip()


def hydrate_mode_metadata(mode: ModeConfig, rustc_version: str) -> ModeConfig:
    """Validate a variant binary and attach reproducibility metadata."""
    if mode.binary_path is None:
        return replace(mode, rustc_version=rustc_version)
    binary = Path(mode.binary_path)
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise ValueError(f"variant binary is not executable: {binary}")
    with binary.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    version = _command_version([str(binary), "--version"]).removeprefix("tilth ")
    return replace(
        mode,
        binary_sha256=digest,
        tilth_version=version,
        rustc_version=rustc_version,
    )


def rustc_version() -> str:
    """Return the compiler version recorded for every experiment arm."""
    return _command_version(["rustc", "--version"])
