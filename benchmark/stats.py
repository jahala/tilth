#!/usr/bin/env python3
"""Thin scipy wrappers for benchmark statistics.

The only new stats surface: every uncertainty number the benchmark reports
routes through here so the methods stay centralized and auditable. scipy is
required (see benchmark/requirements.txt).
"""

import numpy as np
from scipy.stats import binomtest, bootstrap, norm

_STATISTICS = {"mean": np.mean, "median": np.median}


def wilson_interval(successes: int, n: int, confidence: float = 0.95) -> tuple[float, float]:
    """Wilson score interval for a binomial proportion, as (lo, hi) in [0, 1].

    Asymmetric near 0 and 1 — the right tool for accuracy CIs at small n.
    n == 0 returns the maximally-uncertain (0.0, 1.0).
    """
    if n <= 0:
        return (0.0, 1.0)
    ci = binomtest(successes, n).proportion_ci(method="wilson", confidence_level=confidence)
    return (float(ci.low), float(ci.high))


def bootstrap_ci(values: list[float], statistic: str = "mean", confidence: float = 0.95,
                 n_resamples: int = 10_000, seed: int = 0) -> tuple[float, float]:
    """Percentile bootstrap CI of `statistic` over `values`, as (lo, hi).

    statistic is "mean" or "median". The percentile method is used (not BCa):
    BCa returns NaN on degenerate or zero-variance samples. Empty values give
    (0.0, 0.0); a single value or a zero-variance sample gives (point, point).
    """
    stat_fn = _STATISTICS.get(statistic)
    if stat_fn is None:
        raise ValueError(f"unknown statistic {statistic!r}; use 'mean' or 'median'")
    vals = [float(v) for v in values]
    if not vals:
        return (0.0, 0.0)
    point = float(stat_fn(vals))
    if len(vals) < 2 or min(vals) == max(vals):
        return (point, point)
    rng = np.random.default_rng(seed)
    res = bootstrap((vals,), stat_fn, confidence_level=confidence,
                    n_resamples=n_resamples, random_state=rng, method="percentile")
    return (float(res.confidence_interval.low), float(res.confidence_interval.high))


def paired_bootstrap_ci(deltas: list[float], confidence: float = 0.95,
                        n_resamples: int = 10_000, seed: int = 0) -> tuple[float, float]:
    """Percentile bootstrap CI of the MEAN of paired deltas, as (lo, hi).

    A CI that excludes 0 is a paired effect significant at (1 - confidence).
    """
    return bootstrap_ci(deltas, statistic="mean", confidence=confidence,
                        n_resamples=n_resamples, seed=seed)


def ratio_bootstrap_ci(numerators: list[float], denominators: list[float],
                       confidence: float = 0.95, n_resamples: int = 10_000,
                       seed: int = 0) -> tuple[float, float]:
    """Percentile bootstrap CI for the ratio of sums sum(num)/sum(den), as (lo, hi).

    A paired ratio estimator (resamples (num, den) pairs jointly) — the form
    cost-per-correct needs (sum(cost)/sum(correct)), which the scalar bootstrap_ci
    cannot express. sum(denominators) == 0 returns (inf, inf); fewer than 2 pairs
    returns (point, point).
    """
    num = np.asarray([float(x) for x in numerators], dtype=float)
    den = np.asarray([float(x) for x in denominators], dtype=float)
    if num.size != den.size:
        raise ValueError("numerators and denominators must be the same length")
    if num.size == 0 or den.sum() == 0:
        return (float("inf"), float("inf"))
    point = float(num.sum() / den.sum())
    if num.size < 2:
        return (point, point)

    def _ratio(n, d, axis=-1):
        tot_n = np.sum(n, axis=axis)
        tot_d = np.sum(d, axis=axis)
        return np.divide(tot_n, tot_d, out=np.full_like(tot_n, np.inf, dtype=float), where=tot_d > 0)

    rng = np.random.default_rng(seed)
    with np.errstate(invalid="ignore", divide="ignore"):
        res = bootstrap((num, den), _ratio, paired=True, vectorized=True,
                        confidence_level=confidence, n_resamples=n_resamples,
                        random_state=rng, method="percentile")
    return (float(res.confidence_interval.low), float(res.confidence_interval.high))




def min_detectable_effect(n: int, baseline_rate: float, power: float = 0.80,
                          alpha: float = 0.05) -> float:
    """Minimum detectable accuracy gap (proportion points, 0-1) at sample size n.

    Normal approximation for a single proportion around `baseline_rate`:

        MDE = (z_{1-alpha/2} + z_power) * sqrt(p*(1-p)/n)

    the smallest accuracy difference a two-sided test at `alpha` would detect
    with probability `power`. n <= 0 returns 1.0 (nothing detectable); the result
    is capped at 1.0. The normal approximation degrades when `baseline_rate` is
    near 0 or 1 (variance collapses) — read those readouts with that caveat.

    This is a SINGLE-proportion bound, but the power readout consumes it only
    as an optimistic planning guide for a PAIRED variant comparison, whose true
    MDE is larger. The task-clustered paired bootstrap interval remains the
    inferential decision rule.
    """
    if n <= 0:
        return 1.0
    p = min(max(float(baseline_rate), 0.0), 1.0)
    z_alpha = float(norm.ppf(1 - alpha / 2))
    z_power = float(norm.ppf(power))
    mde = (z_alpha + z_power) * float(np.sqrt(p * (1 - p) / n))
    return min(mde, 1.0)
