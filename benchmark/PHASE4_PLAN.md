# Phase 4 — Data adequacy & contamination (execution power-gated)

**Status: design accepted, execution gated.** Do not author or run new tasks
until the trigger below fires. This plan exists so that when the gate opens the
work is already scoped.

## Trigger (explicit)

Execute Phase 4 only when Phase 1's power readout reports insufficient power for
the observed effect. Concretely, run:

```bash
python benchmark/analyze.py benchmark/results/<latest>.jsonl
```

and read the **Power readout by model** line (emitted by
`analyze.py._power_readout`). The gate is OPEN when that line says:

> N INSUFFICIENT for observed effect — grow TASK pool

i.e. the task-clustered paired bootstrap interval includes zero and the
observed task-weighted accuracy gap is below the minimum detectable effect
(MDE) at the current task count N. If instead the line says **effect
SIGNIFICANT**, the paired 95% interval excludes zero — do not spend effort
growing the pool.

Why tasks, not reps: the task is the sampling unit. Reps (held at 3) only shrink
within-task variance; they do not raise N. MDE falls as `1/sqrt(N_tasks)`, so
power is bought by adding TASKS.

## Plan when the gate opens

### 1. Grow the task pool

- **Author post-cutoff / private tasks (contamination fix).** Add tasks over
  code the models cannot have memorized: repos/commits after the model training
  cutoff, or private code. This addresses contamination, not just power — a
  contaminated task inflates baseline accuracy and shrinks the measurable gap.
- **Import curated tasks from ContextBench** (1,136 issues / 66 repos) **into
  THIS harness** to scale N cheaply. Import the tasks as `Task` subclasses with
  `GroundTruth` (substring/alternation) or a `test_command` oracle — do not run
  on ContextBench's own harness (see the spike below).

### 2. Run on the cheap lane, confirm on frontier

- Run volume on the **gpt-5-mini OpenRouter lane** (cheap) to accumulate N fast.
- Reserve **frontier** (`claude -p` / `codex exec`) for a small **confirmatory
  subset** once the cheap lane shows a candidate effect, to confirm the result
  holds at frontier quality without paying frontier cost for the whole pool.
- Re-run `analyze.py` after each batch; stop growing when the task-clustered
  paired interval excludes zero and the readout flips to **effect SIGNIFICANT**.

### 3. (Optional) adopt ContextBench metrics

If richer signal is wanted, adopt ContextBench's recall / precision / efficiency
metrics — computed inside this harness over the imported tasks, alongside the
existing correctness oracle. Optional; not required to close the power gap.

## ContextBench tool-set-swap spike (do FIRST, before any on-ContextBench A/B)

The A/B instrument fixes the model and swaps the TOOL SET (baseline grep/cat/find
vs tilth MCP). `<speculative>` ContextBench's public harness may only swap the
backbone MODEL, not the tool set. Before relying on ContextBench for an A/B, run
a small spike:

1. Read ContextBench's public harness/runner: can an agent's available tools be
   overridden per run (allow/deny tool list, MCP injection), independent of the
   model?
2. Stand up one task two ways — tools=baseline and tools=tilth — same model.
   Confirm the harness actually honours the tool override (tool-call logs differ).
3. **Decision:** if the harness swaps tools cleanly → an on-ContextBench A/B is
   viable. If it only swaps the model → **import ContextBench tasks into this
   harness** (step 1 above) instead; do not run the A/B on ContextBench.

Importing the tasks sidesteps the risk entirely, so the spike only matters if we
want to run on ContextBench's own harness rather than borrow its task pool.

## Provenance

- Research: `.cheese/research/benchmark-model-and-robustness/benchmark-model-and-robustness.md`
  (Q2 — methodology robustness).
- Power readout source: `benchmark/analyze.py` → `_power_readout`,
  `benchmark/paired.py` → `paired_accuracy_delta`, and
  `benchmark/stats.py` → `paired_bootstrap_ci`, `min_detectable_effect`.
