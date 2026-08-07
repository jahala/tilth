import json
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent))

import prepare_experiment
import variants

REFERENCE_SHA = "a" * 40
VARIANT_SHA = "b" * 40
REPOSITORY = "https://github.com/jahala/tilth"


def test_prepare_experiment_resolves_branches_to_pinned_manifest(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    commands: list[list[str]] = []

    def fake_run(command, **kwargs):
        commands.append(command)
        git_ref = command[-1]
        sha = REFERENCE_SHA if git_ref.endswith("/main") else VARIANT_SHA
        return subprocess.CompletedProcess(
            command,
            0,
            stdout=f"{sha}\t{git_ref}\n",
            stderr="",
        )

    monkeypatch.setattr(prepare_experiment.subprocess, "run", fake_run)
    output = tmp_path / "experiment.json"

    manifest = prepare_experiment.prepare_experiment(
        output=output,
        reference_repository=REPOSITORY,
        reference_ref="main",
        variant_repository=None,
        variant_ref="merge-edit-impl",
        arm_order_seed=42,
    )

    assert commands == [
        ["git", "ls-remote", "--exit-code", REPOSITORY, "refs/heads/main"],
        [
            "git",
            "ls-remote",
            "--exit-code",
            REPOSITORY,
            "refs/heads/merge-edit-impl",
        ],
    ]
    assert manifest == json.loads(output.read_text())
    assert manifest == {
        "arm_order_seed": 42,
        "variants": [
            {"name": "no_tilth"},
            {
                "name": "upstream",
                "repository": REPOSITORY,
                "git_ref": "main",
                "git_sha": REFERENCE_SHA,
            },
            {
                "name": "fork",
                "repository": REPOSITORY,
                "git_ref": "merge-edit-impl",
                "git_sha": VARIANT_SHA,
            },
        ],
    }

    experiment = variants.load_experiment(output, variant_root=tmp_path / "variants")
    assert experiment.variants[1].git_ref == "main"
    assert experiment.variants[2].git_ref == "merge-edit-impl"
    assert (
        variants.experiment_modes(experiment, tmp_path / "configs")["fork"].git_ref
        == "merge-edit-impl"
    )


def test_prepare_experiment_accepts_immutable_sha_without_remote_lookup(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    def unexpected_run(*_args, **_kwargs):
        pytest.fail("exact SHAs must not require a remote lookup")

    monkeypatch.setattr(prepare_experiment.subprocess, "run", unexpected_run)

    manifest = prepare_experiment.prepare_experiment(
        output=tmp_path / "experiment.json",
        reference_repository=REPOSITORY,
        reference_ref=REFERENCE_SHA,
        variant_repository="https://github.com/example/tilth",
        variant_ref=VARIANT_SHA,
        arm_order_seed=7,
    )

    assert manifest["variants"][1]["git_sha"] == REFERENCE_SHA
    assert manifest["variants"][2]["git_sha"] == VARIANT_SHA


def test_resolve_git_ref_rejects_missing_branch(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        prepare_experiment.subprocess,
        "run",
        lambda command, **kwargs: subprocess.CompletedProcess(
            command,
            2,
            stdout="",
            stderr="",
        ),
    )

    with pytest.raises(ValueError, match="cannot resolve branch missing"):
        prepare_experiment.resolve_git_ref(REPOSITORY, "missing")
