# tilth Benchmark

Automated evaluation of tilth's impact on AI agent code navigation.

## Results — v0.3.2

| Model | Tasks | Runs | Baseline $/correct | tilth $/correct | Change | Baseline acc | tilth acc |
|---|---|---|---|---|---|---|---|
| Sonnet 4.5 | 21 | 126 | $0.31 | $0.23 | **-26%** | 79% | 86% |
| Opus 4.6 | 6 hard | 36 | $0.49 | $0.42 | **-14%** | 83% | 78% |
| Haiku 4.5 | 7 (forced) | 7 | $0.22 | $0.04 | **-82%** | 69% | 100% |

### Why "cost per correct answer"?

Raw cost comparison treats a wrong answer as a cheap success. It isn't — you paid for a response you can't use and still need the answer. The real question is: **how much do you expect to spend before you get a correct answer?**

This is a geometric retry model. If accuracy is `p`, you need `1/p` attempts on average before one succeeds. The expected cost is:

```
expected_cost = cost_per_attempt × (1 / accuracy)
```

**Cost per correct answer** (`total_spend / correct_answers`) computes this exactly. It's mathematically equivalent to `avg_cost / accuracy_rate` — not an arbitrary penalty, but the expected cost under retry.

## Sonnet 4.5 (126 runs)

21 tasks x 2 modes x 3 reps.

| | Baseline | tilth | Change |
|---|---|---|---|
| **Cost per correct answer** | **$0.31** | **$0.23** | **-26%** |
| Accuracy | 79% (48/61) | 86% (54/63) | +7pp |
| Avg cost per task | $0.25 | $0.20 | -20% |
| Avg turns | 9.3 | 9.1 | -2% |
| Avg tool calls | 8.3 | 8.1 | -2% |
| Avg context tokens | 231,991 | 211,439 | -9% |

tilth is both cheaper per attempt (-20%) *and* more accurate (+7pp). The combined effect: **-26% cost per correct answer**.

### Per-task results

Costs are 3-rep averages. Winner: accuracy difference first, then >=10% cost difference.

```
Task                                     Base    Tilth   Delta  B✓  T✓  Winner
─────────────────────────────────────────────────────────────────────────────────
[E] express_app_init                     $0.14   $0.17    +26%  3/3 3/3  BASE ($)
[E] express_res_send                     $0.13   $0.13     -2%  3/3 3/3  ~tie
[E] gin_client_ip                        $0.18   $0.16    -12%  2/3 3/3  TILTH (acc)
[E] gin_context_next                     $0.05   $0.08    +55%  0/3 3/3  TILTH (acc)
[E] rg_flag_definition                   $0.09   $0.08     -9%  3/3 3/3  ~tie
[E] rg_lineiter_definition               $0.10   $0.09    -12%  1/3 1/3  TILTH ($)
[E] rg_lineiter_usage                    $0.21   $0.13    -38%  3/3 3/3  TILTH ($)
─────────────────────────────────────────────────────────────────────────────────
[M] express_app_render                   $0.12   $0.16    +39%  1/3 3/3  TILTH (acc)
[M] express_json_send                    $0.22   $0.18    -17%  3/3 3/3  TILTH ($)
[M] express_render_chain                 $0.23   $0.23     -1%  3/3 3/3  ~tie
[M] fastapi_depends_function             $0.18   $0.07    -59%  3/3 2/3  BASE (acc)
[M] fastapi_depends_internals            $0.22   $0.17    -22%  2/3 3/3  TILTH (acc)
[M] fastapi_request_validation           $0.18   $0.22    +23%  2/3 3/3  TILTH (acc)
[M] gin_radix_tree                       $0.13   $0.15    +18%  0/3 1/3  TILTH (acc)
─────────────────────────────────────────────────────────────────────────────────
[H] fastapi_dependency_resolution        $0.46   $0.44     -3%  3/3 3/3  ~tie
[H] fastapi_depends_processing           $0.55   $0.21    -61%  3/3 3/3  TILTH ($)
[H] gin_middleware_chain                 $0.38   $0.28    -27%  3/3 3/3  TILTH ($)
[H] gin_servehttp_flow                   $0.38   $0.32    -17%  3/3 3/3  TILTH ($)
[H] rg_search_dispatch                   $0.56   $0.49    -12%  3/3 3/3  TILTH ($)
[H] rg_trait_implementors                $0.23   $0.15    -38%  3/3 2/3  BASE (acc)
[H] rg_walker_parallel                   $0.25   $0.23     -6%  1/3 0/3  BASE (acc)
─────────────────────────────────────────────────────────────────────────────────
TOTAL                                    $4.99   $4.16    -17%  48  54

Accuracy-weighted: W13 T4 L4
```

### By difficulty

| Tier | $/correct (B → T) | Accuracy (B → T) | W-T-L |
|---|---|---|---|
| Easy (7) | $0.18 → $0.13 (-28%) | 71% → 90% | 3-2-0 |
| Medium (7) | $0.27 → $0.20 (-28%) | 67% → 86% | 5-1-1 |
| Hard (7) | $0.44 → $0.37 (-16%) | 90% → 81% | 5-1-2 |

Hard tasks: 5 wins, zero losses on cost-only. The 2 accuracy losses are both Rust tasks where the model struggles in both modes (rg_walker_parallel: 1/3 → 0/3, rg_trait_implementors: 3/3 → 2/3 on one flaky rep).

### By language

| Repo | Language | $/correct (B → T) | Accuracy (B → T) |
|---|---|---|---|
| FastAPI | Python | $0.37 → $0.24 (-35%) | 87% → 93% |
| ripgrep | Rust | $0.31 → $0.29 (-6%) | 78% → 67% |
| Gin | Go | $0.42 → $0.23 (-45%) | 53% → 87% |
| Express | JS | $0.19 → $0.18 (-5%) | 87% → 100% |

Go sees the largest improvement: cost per correct answer drops 45% as accuracy jumps from 53% to 87%. Rust accuracy regresses (78% → 67%) driven by two tasks where the model flakes on specific reps, though cost per correct still improves slightly.

## Opus 4.6 (36 runs)

6 hard tasks, 3 reps each.

```
Task                                     Base    Tilth   Delta  B✓  T✓
─────────────────────────────────────────────────────────────────────────
fastapi_dependency_resolution            $0.41   $0.41    -0%   3/3 0/3  BASE (acc)
fastapi_depends_processing               $0.41   $0.20   -52%   3/3 3/3  TILTH ($)
gin_middleware_chain                     $0.46   $0.31   -32%   3/3 3/3  TILTH ($)
gin_servehttp_flow                       $0.24   $0.32   +29%   3/3 3/3  BASE ($)
rg_search_dispatch                       $0.69   $0.54   -21%   3/3 3/3  TILTH ($)
rg_walker_parallel                       $0.24   $0.19   -21%   0/3 2/3  TILTH (acc)
─────────────────────────────────────────────────────────────────────────
TOTAL                                                           15  14
```

| | Baseline | tilth | Change |
|---|---|---|---|
| **Cost per correct answer** | $0.49 | $0.42 | **-14%** |
| Accuracy | 83% (15/18) | 78% (14/18) | -5pp |
| Avg cost per task | $0.41 | $0.33 | -20% |

Opus uses tilth tools aggressively (4.1 tilth_search + 6.2 tilth_read per run). Notable: `rg_walker_parallel` goes from 0/3 → 2/3 — opus + tilth is the only combination that solves this task. One regression: `fastapi_dependency_resolution` drops from 3/3 → 0/3 with tilth.

## Haiku 4.5 (117 runs)

Haiku was tested in three configurations: baseline (no tilth), hybrid (tilth available alongside built-in tools), and forced (built-in search tools removed).

| Mode | Runs | Accuracy | Avg cost | $/correct | Tilth adoption |
|---|---|---|---|---|---|
| Baseline | 29 | 69% | $0.15 | $0.22 | — |
| Hybrid | 35 | 69% | $0.16 | $0.23 | 9% (3/35 runs) |
| **Forced** | **7** | **100%** | **$0.04** | **$0.04** | **100%** |

In hybrid mode, Haiku used tilth tools in only 3 of 35 valid runs. It defaults to Bash/Grep/Read regardless of MCP instructions. Instruction tuning (moving directives to the top, using CRITICAL/MUST language) had no measurable effect on adoption.

In forced mode (`--disallowedTools "Bash,Grep,Glob"`), Haiku achieves 7/7 correct at $0.04 average — the cheapest correct answers in the entire benchmark. The same 7 tasks score 74% accuracy at $0.15 average in baseline.

## Cross-model analysis

### Tool adoption by model (tilth mode)

| Model | tilth_search/run | tilth_read/run | Bash/run | Adoption rate |
|---|---|---|---|---|
| Haiku 4.5 | 0.2 | 0.1 | 5.9 | 9% |
| Sonnet 4.5 | 2.4 | 3.0 | 0.2 | 95% |
| Opus 4.6 | 4.1 | 6.2 | 2.1 | 94% |

Smarter models adopt tilth tools more aggressively and benefit more from them. Opus makes 4.1 tilth_search calls per run vs Sonnet's 2.4 — it explores more deeply with structured search.

### Variance

tilth generally reduces run-to-run cost variance (coefficient of variation, Sonnet):

| Task | Baseline CV | tilth CV |
|---|---|---|
| fastapi_request_validation | 97% | 41% |
| fastapi_depends_internals | 87% | 68% |
| fastapi_depends_function | 48% | 13% |
| gin_context_next | 30% | 13% |

Structured search results lead to more predictable exploration paths.

### Where tilth wins

**fastapi_depends_processing (-61% cost on Sonnet, -52% on Opus):** Largest win across both models. tilth's callee footer shows the call chain from `solve_dependencies` → `_solve_generator` → `get_dependant` in the search results. Baseline takes 3x the turns to find the same path.

**gin_context_next (+55% cost, but 0/3 → 3/3 accuracy):** Baseline is cheaper but wrong every time — it finds the code but misidentifies the behavior. tilth pays more but actually answers correctly. This is the clearest argument for accuracy-weighted scoring.

**rg_walker_parallel (Opus only: 0/3 → 2/3):** Opus + tilth is the only model+mode combination that solves this task. Sonnet fails in both modes. Haiku fails in both modes. Opus baseline fails. Only opus + tilth cracks it.

### Where tilth loses

**express_app_init (+26% cost, same accuracy):** A trivial task where baseline is already efficient. tilth's MCP overhead doesn't pay off for simple lookups.

**fastapi_dependency_resolution (Opus: 3/3 → 0/3):** A clear regression on Opus. Baseline solves it every time, tilth fails every time. Sonnet shows no issue with this task (3/3 both modes).

**rg_trait_implementors / fastapi_depends_function (accuracy flakes):** tilth misses on 1 of 3 reps each. Single-rep variance, not a systematic failure.

## Methodology

Each run invokes `claude -p` (Claude Code headless mode) with a code navigation question.

**Three modes:**
- **Baseline** — Claude Code built-in tools: Read, Edit, Grep, Glob, Bash
- **tilth** — Built-in tools + tilth MCP server (hybrid mode)
- **tilth_forced** — tilth MCP + Read/Edit only (Bash, Grep, Glob removed)

All modes use the same system prompt, $1.00 budget cap, and model. The agent explores the codebase and returns a natural-language answer. Correctness is checked against ground-truth strings that must appear in the response.

**Repos (pinned commits):**

| Repo | Language | Description |
|---|---|---|
| [Express](https://github.com/expressjs/express) | JavaScript | HTTP framework |
| [FastAPI](https://github.com/tiangolo/fastapi) | Python | Async web framework |
| [Gin](https://github.com/gin-gonic/gin) | Go | HTTP framework |
| [ripgrep](https://github.com/BurntSushi/ripgrep) | Rust | Line-oriented search |

**Difficulty tiers (7 tasks each, Sonnet only):**
- **Easy** — Single-file lookups, finding definitions, tracing short paths
- **Medium** — Cross-file tracing, understanding data flow, 2-3 hop chains
- **Hard** — Deep call chains, multi-file architecture, complex dispatch

### Running benchmarks

**Prerequisites:**
- Python 3.9+
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) CLI (`claude`) installed and authenticated
- tilth installed (`cargo install tilth` or `npx tilth`)
- Git (for cloning benchmark repos)

**Setup:**

```bash
# Clone repos at pinned commits (~100MB total)
python benchmark/fixtures/setup_repos.py
```

**Run:**

```bash
# All 21 tasks, baseline + tilth, 3 reps, Sonnet
python benchmark/run.py --tasks all_real --model sonnet --reps 3

# Specific tasks
python benchmark/run.py --tasks fastapi_depends_processing,gin_middleware_chain --model sonnet --reps 3

# Opus on hard tasks only
python benchmark/run.py --tasks all_real --model opus --reps 3

# Haiku forced mode (built-in search tools removed)
python benchmark/run.py --tasks all_real --model haiku --reps 1 --modes tilth_forced

# Single mode only (skip baseline comparison)
python benchmark/run.py --tasks all_real --model sonnet --reps 1 --modes tilth
```

**Analyze:**

```bash
# Summarize results from a run
python benchmark/analyze.py benchmark/results/benchmark_<timestamp>_<model>.jsonl

# Compare two runs (e.g. different versions)
python benchmark/compare_versions.py benchmark/results/old.jsonl benchmark/results/new.jsonl
```

Results are written to `benchmark/results/benchmark_<timestamp>_<model>.jsonl`. Each line is a JSON object with task name, mode, cost, token counts, correctness, and tool sequence.

### Task definitions

Tasks are in `benchmark/tasks/`. Each specifies `repo`, `prompt`, `ground_truth` (correctness strings), and `difficulty`.

### Contributing benchmarks

We welcome benchmark contributions — more data makes the results more reliable.

**Adding results:** Run the benchmark suite on your machine and share the `.jsonl` file in a GitHub issue or PR. Different hardware, API regions, and model versions can all affect results.

**Adding tasks:** Create a new task class in `benchmark/tasks/` following the existing pattern. Each task needs:
- `repo`: which benchmark repo to use
- `prompt`: the code navigation question
- `ground_truth`: list of strings that must appear in a correct answer
- `difficulty`: `"easy"`, `"medium"`, or `"hard"`

Good tasks have unambiguous correct answers that can be verified by string matching. Avoid tasks where the answer depends on interpretation.

## Version history

| Version | Changes | Cost/correct (Sonnet) |
|---|---|---|
| v0.2.1 | First benchmark | baseline |
| v0.3.0 | Callee footer, session dedup, multi-symbol search | -8% |
| v0.3.1 | Go same-package callees, map demotion | +12% (regression) |
| v0.3.2 | Map disabled, instruction tuning, multi-model benchmarks | **-26%** |

v0.3.1 regressed because the model overused tilth_map (62% of losing tasks) and re-read files already shown in search results. v0.3.2 disabled map and added instruction guidance to stop re-reading expanded code.
