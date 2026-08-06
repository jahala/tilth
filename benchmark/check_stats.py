#!/usr/bin/env python3
"""Standalone checks for the benchmark stats layer — no pytest.

Exercises stats.py, the paired-A/B pairing logic, the grader alternation, and
the offline re-grade recovery path on hand-checked inputs. Run after installing
the deps:

    pip install -r benchmark/requirements.txt
    python benchmark/check_stats.py

Exits 0 on success; raises AssertionError (non-zero) on the first failure.
"""

import sys
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).parent))

import stats
import regrade
from paired import pair_ab
from tasks.base import GroundTruth, required_matches


def approx(a: float, b: float, tol: float = 1e-9) -> bool:
    return abs(a - b) <= tol


def check_stats() -> None:
    # Wilson: brackets the point estimate, asymmetric near the edge.
    lo, hi = stats.wilson_interval(8, 10)
    assert lo < 0.8 < hi, (lo, hi)
    assert (0.8 - lo) > (hi - 0.8), "Wilson should be asymmetric near 0.8"
    assert stats.wilson_interval(0, 0) == (0.0, 1.0)
    full_lo, full_hi = stats.wilson_interval(10, 10)
    assert full_hi == 1.0 and full_lo < 1.0

    # bootstrap_ci: edges and a normal case.
    assert stats.bootstrap_ci([]) == (0.0, 0.0)
    assert stats.bootstrap_ci([5.0]) == (5.0, 5.0)
    assert stats.bootstrap_ci([3.0, 3.0, 3.0]) == (3.0, 3.0)
    blo, bhi = stats.bootstrap_ci([1, 2, 3, 4, 5, 6, 7, 8])
    assert blo < 4.5 < bhi, (blo, bhi)
    try:
        stats.bootstrap_ci([1, 2, 3], statistic="bogus")
        raise AssertionError("expected ValueError for bad statistic")
    except ValueError:
        pass

    # paired_bootstrap_ci: CI around the mean delta.
    plo, phi = stats.paired_bootstrap_ci([0.1, -0.2, 0.3, 0.05, -0.1, 0.4, 0.0, 0.2])
    assert plo < (0.75 / 8) < phi, (plo, phi)

    # McNemar exact.
    p, direction = stats.mcnemar_exact(1, 9)
    assert p < 0.05 and direction == "tilth", (p, direction)
    assert stats.mcnemar_exact(9, 1)[1] == "baseline"
    assert stats.mcnemar_exact(0, 0) == (1.0, "tie")
    assert approx(stats.mcnemar_exact(5, 5)[0], 1.0)

    # MDE: positive, capped at 1.0, larger N -> smaller MDE, n<=0 -> 1.0.
    assert stats.min_detectable_effect(0, 0.5) == 1.0
    mde_small = stats.min_detectable_effect(10, 0.5)
    mde_large = stats.min_detectable_effect(200, 0.5)
    assert 0 < mde_large < mde_small <= 1.0, (mde_small, mde_large)
    print("  stats.py ......... OK")


def check_pairing() -> None:
    # baseline vs tilth across 3 reps; rep2 baseline ERRORED (no cost, correct=False).
    runs = []
    for rep in range(3):
        runs.append({"task": "t1", "model": "sonnet", "mode": "tilth",
                     "repetition": rep, "correct": True, "total_cost_usd": 0.05})
    for rep in (0, 1):
        runs.append({"task": "t1", "model": "sonnet", "mode": "baseline",
                     "repetition": rep, "correct": rep == 0, "total_cost_usd": 0.10})
    # errored baseline rep2: counts as incorrect, no cost.
    runs.append({"task": "t1", "model": "sonnet", "mode": "baseline",
                 "repetition": 2, "error": "timeout", "correct": False})

    pairs = pair_ab(runs)
    tuples = pairs[("t1", "claude-sonnet-4-6")]
    assert len(tuples) == 3, "all 3 reps pair, including the errored one"
    # discordances: tilth correct on all 3; baseline correct only rep0.
    # rep1: baseline wrong, tilth right -> c. rep2 (error): baseline wrong, tilth right -> c.
    c = sum(1 for (_r, bc, tc, _bk, _tk) in tuples if tc and not bc)
    b = sum(1 for (_r, bc, tc, _bk, _tk) in tuples if bc and not tc)
    assert (b, c) == (0, 2), (b, c)
    # errored rep drops from cost deltas (base cost None) -> only 2 cost deltas.
    cost_deltas = [tk - bk for (_r, _bc, _tc, bk, tk) in tuples if bk is not None and tk is not None]
    assert len(cost_deltas) == 2, cost_deltas
    print("  pairing .......... OK")


def check_alternation() -> None:
    # Backward compatible: an entry with no "|" matches exactly as before.
    assert required_matches("ServeHTTP", "servehttp dispatches") is True
    assert required_matches("absent", "nope") is False
    # Alternation: OR within an entry.
    assert required_matches("foo|bar", "only bar here") is True
    assert required_matches("foo|bar", "only baz here") is False
    print("  alternation ...... OK")


def check_regrade() -> None:
    fake = SimpleNamespace(
        mutations=[], test_command=[], task_type="read",
        ground_truth=GroundTruth(required_strings=["alpha|omega"]),
    )
    regrade.TASKS["__fake__"] = fake
    try:
        # Recovery: stored incorrect (old exact-match on 'alpha' failed), but the
        # answer contains 'omega' -> alternation now passes.
        recovered, regressed, regradeable, _skipped = regrade.regrade_file([
            {"task": "__fake__", "correct": False, "result_text": "mentions omega", "repetition": 0},
            {"task": "__fake__", "correct": True, "result_text": "alpha is here", "repetition": 1},
        ])
        assert regradeable == 2, regradeable
        assert len(recovered) == 1 and recovered[0]["repetition"] == 0, recovered
        assert not regressed, regressed

        # Regression detection: stored correct but answer matches neither alt.
        _rec, regressed2, _rg, _sk = regrade.regrade_file([
            {"task": "__fake__", "correct": True, "result_text": "nothing relevant", "repetition": 2},
        ])
        assert len(regressed2) == 1, regressed2

        # Mutation/edit/error/no-answer records are skipped, not graded.
        muta = SimpleNamespace(mutations=[object()], test_command=["true"], task_type="read",
                               ground_truth=GroundTruth(required_strings=["x"]))
        regrade.TASKS["__muta__"] = muta
        try:
            _r, _rg2, regradeable2, skipped2 = regrade.regrade_file([
                {"task": "__muta__", "correct": False, "result_text": "x", "repetition": 0},
                {"task": "__fake__", "error": "timeout", "correct": False, "repetition": 0},
                {"task": "__unknown__", "correct": False, "result_text": "x", "repetition": 0},
            ])
            assert regradeable2 == 0 and skipped2 == 3, (regradeable2, skipped2)
        finally:
            del regrade.TASKS["__muta__"]
    finally:
        del regrade.TASKS["__fake__"]
    print("  regrade .......... OK")


def check_ratio_bootstrap() -> None:
    # CI brackets the ratio of sums sum(cost)/sum(correct).
    lo, hi = stats.ratio_bootstrap_ci([0.1, 0.2, 0.05, 0.3, 0.15, 0.25, 0.12, 0.08],
                                      [1, 1, 0, 1, 1, 0, 1, 1])
    point = (0.1 + 0.2 + 0.05 + 0.3 + 0.15 + 0.25 + 0.12 + 0.08) / 6
    assert lo < point < hi, (lo, point, hi)
    # Degenerate: zero denominator, single pair, empty, length mismatch.
    assert stats.ratio_bootstrap_ci([0.1, 0.2], [0, 0]) == (float("inf"), float("inf"))
    assert stats.ratio_bootstrap_ci([0.1], [1]) == (0.1, 0.1)
    assert stats.ratio_bootstrap_ci([], []) == (float("inf"), float("inf"))
    try:
        stats.ratio_bootstrap_ci([1, 2], [1])
        raise AssertionError("expected ValueError on length mismatch")
    except ValueError:
        pass
    print("  ratio_bootstrap .. OK")


def check_analyze_metrics() -> None:
    import analyze
    runs = [
        {"total_cost_usd": 0.10, "correct": True},
        {"total_cost_usd": 0.20, "correct": False},
        {"total_cost_usd": 0.30, "correct": True},
    ]
    value, lo, hi = analyze.cost_per_correct(runs)
    assert approx(value, 0.60 / 2), value  # total $0.60 over 2 correct
    assert lo <= value <= hi, (lo, value, hi)
    assert analyze.cost_per_correct([{"total_cost_usd": 0.1, "correct": False}])[0] == float("inf")
    assert analyze.cost_per_correct([])[0] == float("inf")
    pct, clo, chi = analyze.correctness_with_ci(
        [{"correct": True}, {"correct": True}, {"correct": False}, {"correct": False}])
    assert approx(pct, 50.0), pct
    assert clo < pct < chi, (clo, pct, chi)
    assert analyze.correctness_with_ci([]) == (0.0, 0.0, 0.0)
    print("  analyze metrics .. OK")


def check_power_readout() -> None:
    import analyze

    def runs_for(pattern):
        out = []
        for i, (bc, tc) in enumerate(pattern):
            out.append({"task": f"t{i}", "model": "m", "mode": "baseline",
                        "repetition": 0, "correct": bc, "total_cost_usd": 0.1})
            out.append({"task": f"t{i}", "model": "m", "mode": "tilth",
                        "repetition": 0, "correct": tc, "total_cost_usd": 0.05})
        return out

    # Significant: baseline 2/10, tilth 10/10 -> b=0, c=8, McNemar p<0.05.
    sig = runs_for([(True, True), (True, True)] + [(False, True)] * 8)
    text = "\n".join(analyze._power_readout(sig))
    assert "SIGNIFICANT" in text, text

    # Insufficient: 4 tasks, small effect, not significant, observed < MDE.
    insuff = runs_for([(True, True), (True, False), (False, True), (False, True)])
    text2 = "\n".join(analyze._power_readout(insuff))
    assert "N INSUFFICIENT" in text2, text2

    # No paired data -> graceful message.
    only_base = [{"task": "t", "model": "m", "mode": "baseline",
                  "repetition": 0, "correct": True, "total_cost_usd": 0.1}]
    text3 = "\n".join(analyze._power_readout(only_base))
    assert "unavailable" in text3, text3
    print("  power readout .... OK")


def main() -> None:
    print("benchmark stats-layer checks:")
    check_stats()
    check_pairing()
    check_alternation()
    check_regrade()
    check_ratio_bootstrap()
    check_analyze_metrics()
    check_power_readout()
    print("ALL CHECKS PASSED")


if __name__ == "__main__":
    main()
