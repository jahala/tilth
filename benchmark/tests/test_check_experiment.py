import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent))

import check_experiment
import variants


def _manifest(tmp_path: Path) -> Path:
    path = tmp_path / "experiment.json"
    path.write_text(
        json.dumps(
            {
                "arm_order_seed": 42,
                "variants": [
                    {"name": "no_tilth"},
                    {
                        "name": "upstream",
                        "repository": "https://github.com/jahala/tilth",
                        "git_ref": "main",
                        "git_sha": "a" * 40,
                    },
                    {
                        "name": "fork",
                        "repository": "https://github.com/jahala/tilth",
                        "git_ref": "merge-edit-impl",
                        "git_sha": "b" * 40,
                    },
                ],
            }
        )
    )
    return path


def _records(manifest_path: Path, variant_root: Path) -> list[dict]:
    experiment = variants.load_experiment(manifest_path, variant_root=variant_root)
    arm_order = ["fork", "no_tilth", "upstream"]
    records = []
    for arm_index, mode in enumerate(arm_order):
        variant = next(item for item in experiment.variants if item.name == mode)
        records.append(
            {
                "task": "find_definition",
                "mode": mode,
                "model_alias": "haiku",
                "repetition": 0,
                "arm_order_seed": experiment.arm_order_seed,
                "arm_order": arm_order,
                "arm_order_index": arm_index,
                "variant": {
                    "label": mode,
                    "repository": variant.repository,
                    "git_ref": variant.git_ref,
                    "git_sha": variant.git_sha,
                    "binary_path": str(variant.binary_path)
                    if variant.binary_path
                    else None,
                    "binary_sha256": "c" * 64 if variant.binary_path else None,
                },
                "correct": True,
            }
        )
    return records


def _write_records(path: Path, records: list[dict]) -> None:
    path.write_text("".join(f"{json.dumps(record)}\n" for record in records))


def test_validate_experiment_run_accepts_complete_error_free_block(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    variant_root = tmp_path / "variants"
    monkeypatch.setenv("TILTH_BENCH_VARIANT_ROOT", str(variant_root))
    manifest_path = _manifest(tmp_path)
    result_path = tmp_path / "results.jsonl"
    _write_records(result_path, _records(manifest_path, variant_root))

    summary = check_experiment.validate_experiment_run(result_path, manifest_path)

    assert summary.rows == 3
    assert summary.blocks == 1


def test_validate_experiment_run_rejects_tilth_arm_that_ran_native_only(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    """A tilth-armed row whose recorded session tools lack mcp__tilth__ ran
    native-only — the exact --safe-mode failure that invalidated a full run."""
    variant_root = tmp_path / "variants"
    monkeypatch.setenv("TILTH_BENCH_VARIANT_ROOT", str(variant_root))
    manifest_path = _manifest(tmp_path)
    result_path = tmp_path / "results.jsonl"
    records = _records(manifest_path, variant_root)
    for record in records:
        if record["mode"] == "fork":
            record["available_tools"] = ["Bash", "Edit", "Glob", "Grep", "Read"]
    _write_records(result_path, records)

    with pytest.raises(ValueError, match="ran native-only"):
        check_experiment.validate_experiment_run(result_path, manifest_path)


def test_validate_experiment_run_accepts_tilth_arm_with_recorded_tilth_tools(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    variant_root = tmp_path / "variants"
    monkeypatch.setenv("TILTH_BENCH_VARIANT_ROOT", str(variant_root))
    manifest_path = _manifest(tmp_path)
    result_path = tmp_path / "results.jsonl"
    records = _records(manifest_path, variant_root)
    for record in records:
        if record["mode"] != "no_tilth":
            record["available_tools"] = ["Read", "mcp__tilth__tilth_search"]
    _write_records(result_path, records)

    summary = check_experiment.validate_experiment_run(result_path, manifest_path)

    assert summary.rows == 3


def test_validate_experiment_run_rejects_a_missing_complete_block(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    variant_root = tmp_path / "variants"
    monkeypatch.setenv("TILTH_BENCH_VARIANT_ROOT", str(variant_root))
    manifest_path = _manifest(tmp_path)
    result_path = tmp_path / "results.jsonl"
    _write_records(result_path, _records(manifest_path, variant_root))

    with pytest.raises(
        ValueError,
        match="result blocks do not match expected schedule",
    ):
        check_experiment.validate_experiment_run(
            result_path,
            manifest_path,
            expected_tasks=["find_definition", "markdown_section"],
            expected_models=["haiku"],
            expected_repetitions=1,
        )


@pytest.mark.parametrize(
    ("mutate", "message"),
    [
        (lambda rows: rows[0].update(error="not logged in"), "contains 1 error row"),
        (lambda rows: rows.pop(), "does not contain each experiment arm exactly once"),
        (
            lambda rows: rows[0]["variant"].update(git_sha="d" * 40),
            "variant metadata does not match manifest",
        ),
        (
            lambda rows: rows[0].update(arm_order_seed=99),
            "arm order seed does not match manifest",
        ),
    ],
)
def test_validate_experiment_run_rejects_incomplete_or_error_results(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    mutate,
    message: str,
) -> None:
    variant_root = tmp_path / "variants"
    monkeypatch.setenv("TILTH_BENCH_VARIANT_ROOT", str(variant_root))
    manifest_path = _manifest(tmp_path)
    records = _records(manifest_path, variant_root)
    mutate(records)
    result_path = tmp_path / "results.jsonl"
    _write_records(result_path, records)

    with pytest.raises(ValueError, match=message):
        check_experiment.validate_experiment_run(result_path, manifest_path)
