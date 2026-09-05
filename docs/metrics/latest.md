# tilth — latest metric

**Metric.** Cost per correct answer: total spend divided by correct answers, the expected cost of being right under retry. Same yardstick as copeca.

**Latest measured result.** tilth v0.5.0, measured 2026-03-08 with the in-repo benchmark (`benchmark/`): 26 code-navigation tasks across ripgrep, gin, express, fastapi; Claude Code built-in tools as baseline, built-in tools plus tilth MCP as treatment.

| Model | Runs | Baseline $/correct | tilth $/correct | Change | Baseline accuracy | tilth accuracy |
|---|---|---|---|---|---|---|
| Sonnet 4.6 | 86 | $0.26 | $0.15 | −44% | 84% | 94% |
| Opus 4.6 | 25 | $0.22 | $0.14 | −39% | 91% | 92% |
| Haiku 4.5 | 49 | $0.12 | $0.08 | −38% | 54% | 73% |
| average | 160 | $0.20 | $0.12 | −40% | 76% | 86% |

**Confidence interval.** None: the in-repo benchmark of that date reported point estimates only. Per-task results and methodology are in `benchmark/README.md`.

**Signed artifact.** None yet. The next measurement is the copeca run of `docs/ab/skill-vs-mcp-2026-09.md` (baseline versus MCP with the pointer versus CLI plus skill, Sonnet, three repetitions); when it lands, this file records its cost per correct with the paired bootstrap interval, the tilth version measured, the date, and the path to the signed `.copeca` artifact, and the table above becomes history.

**Versions cited elsewhere.** README and the landing page cite the v0.5.0 figures and say so; they keep the version they measured.
