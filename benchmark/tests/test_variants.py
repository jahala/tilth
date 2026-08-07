import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent))

import build_variants
import variants

UPSTREAM_SHA = "ad9eb2cdb90a4333c4eec337ff7499e1867d248f"
FORK_SHA = "4bb885e76d4ade1babf51ee820c7588114df9ea9"


def _manifest() -> dict:
    return {
        "arm_order_seed": 163162,
        "variants": [
            {"name": "no_tilth"},
            {
                "name": "upstream",
                "repository": "https://github.com/jahala/tilth",
                "git_sha": UPSTREAM_SHA,
            },
            {
                "name": "fork",
                "repository": "https://github.com/paulnsorensen/tilth",
                "git_sha": FORK_SHA,
            },
        ],
    }


def test_load_experiment_derives_separate_pinned_binary_paths(tmp_path: Path) -> None:
    manifest_path = tmp_path / "experiment.json"
    manifest_path.write_text(json.dumps(_manifest()))
    variant_root = tmp_path / "installed"

    experiment = variants.load_experiment(manifest_path, variant_root=variant_root)

    assert experiment.arm_order_seed == 163162
    assert [variant.name for variant in experiment.variants] == [
        "no_tilth",
        "upstream",
        "fork",
    ]
    assert experiment.variants[0].binary_path is None
    assert (
        experiment.variants[1].binary_path
        == (variant_root / f"upstream-{UPSTREAM_SHA}" / "bin" / "tilth").resolve()
    )
    assert (
        experiment.variants[2].binary_path
        == (variant_root / f"fork-{FORK_SHA}" / "bin" / "tilth").resolve()
    )
    assert experiment.variants[1].binary_path != experiment.variants[2].binary_path


@pytest.mark.parametrize(
    ("change", "message"),
    [
        (
            lambda data: data["variants"].append({"name": "fork"}),
            "duplicate variant name",
        ),
        (lambda data: data["variants"][1].update(git_sha="main"), "40-character"),
        (lambda data: data["variants"][1].update(extra=True), "unknown keys"),
        (lambda data: data.update(extra=True), "unknown keys"),
    ],
)
def test_load_experiment_rejects_ambiguous_or_unpinned_variants(
    tmp_path: Path,
    change,
    message: str,
) -> None:
    data = _manifest()
    change(data)
    manifest_path = tmp_path / "experiment.json"
    manifest_path.write_text(json.dumps(data))

    with pytest.raises(ValueError, match=message):
        variants.load_experiment(manifest_path, variant_root=tmp_path / "installed")


def test_experiment_modes_write_absolute_runner_configs(tmp_path: Path) -> None:
    manifest_path = tmp_path / "experiment.json"
    manifest_path.write_text(json.dumps(_manifest()))
    experiment = variants.load_experiment(
        manifest_path,
        variant_root=tmp_path / "installed",
    )

    modes = variants.experiment_modes(experiment, tmp_path / "configs")

    assert set(modes) == {"no_tilth", "upstream", "fork"}
    assert modes["no_tilth"].mcp_config_path is None
    assert modes["no_tilth"].opencode_config_path is not None
    assert (
        json.loads(Path(modes["no_tilth"].opencode_config_path).read_text())["mcp"]
        == {}
    )

    upstream_binary = str(experiment.variants[1].binary_path)
    claude_config = json.loads(Path(modes["upstream"].mcp_config_path).read_text())
    opencode_config = json.loads(
        Path(modes["upstream"].opencode_config_path).read_text()
    )
    assert claude_config == {
        "mcpServers": {
            "tilth": {
                "type": "stdio",
                "command": upstream_binary,
                "args": ["--mcp", "--edit"],
            }
        }
    }
    assert opencode_config["mcp"]["tilth"]["command"] == [
        upstream_binary,
        "--mcp",
        "--edit",
    ]
    assert modes["upstream"].repository == "https://github.com/jahala/tilth"
    assert modes["upstream"].git_sha == UPSTREAM_SHA
    assert modes["upstream"].binary_path == upstream_binary


def test_arm_order_is_deterministic_per_block_and_contains_each_arm_once() -> None:
    arms = ["no_tilth", "upstream", "fork"]

    first = variants.randomized_arm_order(
        arms,
        seed=163162,
        task="find_definition",
        model="sonnet",
        repetition=2,
    )
    repeated = variants.randomized_arm_order(
        arms,
        seed=163162,
        task="find_definition",
        model="sonnet",
        repetition=2,
    )
    another_block = variants.randomized_arm_order(
        arms,
        seed=163162,
        task="find_definition",
        model="sonnet",
        repetition=3,
    )

    assert first == repeated
    assert sorted(first) == sorted(arms)
    assert first != another_block


def test_install_variants_builds_each_pin_into_its_own_root(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    manifest_path = tmp_path / "experiment.json"
    manifest_path.write_text(json.dumps(_manifest()))
    experiment = variants.load_experiment(
        manifest_path,
        variant_root=tmp_path / "installed",
    )
    commands: list[list[str]] = []

    def fake_run(command, **kwargs):
        commands.append(command)
        root = Path(command[command.index("--root") + 1])
        binary = root / "bin" / "tilth"
        binary.parent.mkdir(parents=True)
        binary.write_text("binary")
        binary.chmod(0o755)

    monkeypatch.setattr(build_variants.subprocess, "run", fake_run)

    installed = build_variants.install_variants(experiment)

    assert installed == {
        variant.name: variant.binary_path
        for variant in experiment.variants
        if variant.binary_path is not None
    }
    assert commands == [
        [
            "cargo",
            "install",
            "--git",
            variant.repository,
            "--rev",
            variant.git_sha,
            "--root",
            str(variant.binary_path.parent.parent),
            "--locked",
            "--force",
            "tilth",
        ]
        for variant in experiment.variants
        if variant.binary_path is not None
    ]
