---
name: tilth-benchmark
description: Runs reproducible three-arm tilth benchmarks against a configurable candidate Git branch and a reference branch. Use when the user says "benchmark this tilth branch", "compare a PR branch to upstream", "run the three-way benchmark", "smoke the benchmark", or wants a tilth benchmark embedded in a routine.
---

# tilth Branch Benchmark

Compare a candidate tilth branch with a reference branch and the no-tilth control. Pin both mutable refs to immutable SHAs before building or making paid model calls.

The analyzer calls the candidate arm `fork` for compatibility. The candidate may be a branch in the same repository as the reference, such as `jahala/tilth:merge-edit-impl` against `jahala/tilth:main`.

## Required Inputs

Collect or derive these before running:

- candidate repository and ref; the ref is required
- reference repository and ref; default to `https://github.com/jahala/tilth` and `main`
- explicit comma-separated task names
- explicit comma-separated model aliases from `benchmark/config.py`
- repetitions, at least 1
- maximum permitted benchmark cells

Never default a routine to `--tasks all` or `--models all`. A routine must make its paid workload visible in its configuration.

## Plan-only Requests

When the user asks for a plan, commands, or a dry run, return the bounded runbook and cell calculation without invoking authentication, Git resolution, builds, fixtures, the runner, validator, or analyzer. Preserve unresolved values as explicit placeholders rather than inventing tasks, repetitions, or a ceiling.

## Existing Result Analysis

When the user supplies an existing JSONL result and its experiment manifest and explicitly says not to rerun, skip authentication, ref resolution, builds, fixture setup, and the benchmark runner. Validate with `benchmark/check_experiment.py <result> <manifest> --tasks <tasks> --models <models> --reps <repetitions>`, then run `benchmark/analyze.py <result>` only if validation succeeds. If the original schedule is unavailable, omit the expectation flags and label whole-schedule completeness unverified.

## New Run

1. Confirm the working tree is the intended tilth checkout. Do not stage or commit generated benchmark artifacts.
2. Map each selected model through `RUNNERS` in `benchmark/config.py`. Confirm the corresponding CLI exists and its noninteractive authentication status succeeds. For Claude lanes, run `claude auth status`. Hosted routines must not assume local OAuth or keychain credentials are present.
3. Compute `cells = task_count × model_count × repetitions × 3`. Stop before any model call when this exceeds the configured maximum.
4. Create a temporary manifest by resolving both refs:

```bash
mkdir -p /tmp/tilth-benchmark
python benchmark/prepare_experiment.py \
  --reference-repository <reference-repository> \
  --reference-ref <reference-ref> \
  --variant-repository <candidate-repository> \
  --variant-ref <candidate-ref> \
  --output /tmp/tilth-benchmark/experiment.json
```

5. Read the generated manifest. Report both requested refs and resolved SHAs before continuing. Treat ref-resolution failure or an unexpected repository/SHA as a hard stop.
6. Build the two pinned binaries:

```bash
python benchmark/build_variants.py /tmp/tilth-benchmark/experiment.json
```

7. Set up only fixtures required by the selected tasks. Use `python benchmark/fixtures/setup_repos.py` when a selected real-repository fixture is missing; use `python benchmark/fixtures/setup.py` for the synthetic fixture.
8. Run the exact bounded experiment:

```bash
python benchmark/run.py \
  --experiment /tmp/tilth-benchmark/experiment.json \
  --tasks <task-a,task-b> \
  --models <model-a,model-b> \
  --reps <repetitions> \
  --max-cells <maximum-cells>
```

9. Capture the result path printed by the runner. The runner records cell failures and finishes the schedule, so a zero exit code is not evidence of a valid run.
10. Validate the result before analysis:

```bash
python benchmark/check_experiment.py \
  <result.jsonl> \
  /tmp/tilth-benchmark/experiment.json \
  --tasks <task-a,task-b> \
  --models <model-a,model-b> \
  --reps <repetitions>
```

11. Only after validation succeeds, analyze it:

```bash
python benchmark/analyze.py <result.jsonl>
```

## Routine Result

Return a compact record containing:

- reference repository/ref and resolved SHA
- candidate repository/ref and resolved SHA
- tasks, models, repetitions, and planned/validated cell counts
- manifest, JSONL, and report paths
- validation status and any error-row count
- primary `fork` versus `upstream` accuracy and cost-per-correct metrics
- `fork` versus `no_tilth` and `upstream` versus `no_tilth` guardrails

If preflight, build, run validation, or analysis fails, return the failing command and concise error. Do not present partial results as a benchmark conclusion.

## Discipline

**Iron Law:** No paid benchmark cell runs until both Git refs are pinned to SHAs and the explicit three-arm cell count is within the configured ceiling.

Stop on these red flags:

- a mutable ref is passed directly to a build step
- candidate ref is omitted or silently replaced by the current checkout
- `all` expands the task or model set inside a routine
- runner authentication is assumed rather than checked
- result analysis starts before error and block validation
- generated manifests, configs, streams, results, or reports are staged

| Rationalization | Response |
|---|---|
| "It is only a smoke run." | Pin refs and enforce the cell ceiling; smoke runs still spend money and produce comparative claims. |
| "The branch will not move during the run." | Resolve it once anyway; the recorded SHA is the experiment identity. |
| "The runner exited zero." | Validate the JSONL; cell errors are deliberately recorded without aborting the schedule. |
| "Both arms use the same repository, so this is not a fork." | Keep the candidate in the `fork` arm; it is the analyzer's compatibility label, not a repository ownership claim. |
| "The routine environment probably has Claude auth." | Run the auth preflight and stop loudly if credentials are unavailable. |
