# tilth Benchmark

Automated evaluation of tilth's impact on AI agent code navigation.

## Results — v0.4.1

| Model | Tasks | Runs | Baseline $/correct | tilth $/correct | Change | Baseline acc | tilth acc |
|---|---|---|---|---|---|---|---|
| Sonnet 4.5 | 26 | 52 | $0.26 | $0.19 | **-29%** | 96% | 92% |
| Opus 4.6 | 5 hard | 10 | $0.29 | $0.23 | **-22%** | 100% | 100% |
| Haiku 4.5 | 26 | 52 | $0.17 | $0.19 | +12% | 58% | 69% |
| **Average** | | **114** | **$0.23** | **$0.19** | **-18%** | **79%** | **82%** |

### Why "cost per correct answer"?

Raw cost comparison treats a wrong answer as a cheap success. It isn't — you paid for a response you can't use and still need the answer. The real question is: **how much do you expect to spend before you get a correct answer?**

This is a geometric retry model. If accuracy is `p`, you need `1/p` attempts on average before one succeeds. The expected cost is:

```
expected_cost = cost_per_attempt × (1 / accuracy)
```

**Cost per correct answer** (`total_spend / correct_answers`) computes this exactly. It's mathematically equivalent to `avg_cost / accuracy_rate` — not an arbitrary penalty, but the expected cost under retry.

## Sonnet 4.5 (52 runs)

26 tasks across 4 repos. 26 baseline + 26 tilth runs. 98% tilth tool adoption (184/188 tool calls used tilth).

| | Baseline | tilth | Change |
|---|---|---|---|
| **Cost per correct answer** | **$0.26** | **$0.19** | **-29%** |
| Accuracy | 96% (25/26) | 92% (24/26) | -4pp |
| Avg cost per task | $0.25 | $0.17 | -32% |
| Avg turns | 9.3 | 8.2 | -12% |
| Avg tool calls | 8.3 | 7.2 | -13% |
| Avg context tokens | 225,570 | 163,521 | -28% |

tilth is cheaper per attempt (-32%) with near-identical accuracy (-4pp). The combined effect: **-29% cost per correct answer**.

### Per-task results

```
Task                                       Base    Tilth   Delta  B✓  T✓  Winner
─────────────────────────────────────────────────────────────────────────────────
fastapi_depends_function                  $0.34   $0.09   -74%  1/1 1/1  TILTH ($)
fastapi_depends_internals                 $0.31   $0.08   -73%  1/1 1/1  TILTH ($)
rg_lineiter_usage                         $0.30   $0.09   -69%  1/1 1/1  TILTH ($)
rg_trait_implementors                     $0.29   $0.10   -65%  1/1 1/1  TILTH ($)
fastapi_depends_processing                $0.51   $0.21   -58%  1/1 1/1  TILTH ($)
find_definition                           $0.10   $0.06   -43%  1/1 1/1  TILTH ($)
gin_client_ip                             $0.38   $0.22   -43%  1/1 1/1  TILTH ($)
fastapi_request_validation                $0.26   $0.16   -38%  1/1 1/1  TILTH ($)
fastapi_dependency_resolution             $0.45   $0.28   -37%  1/1 1/1  TILTH ($)
read_large_file                           $0.12   $0.08   -33%  1/1 1/1  TILTH ($)
rg_walker_parallel                        $0.28   $0.19   -32%  1/1 1/1  TILTH ($)
edit_task                                 $0.09   $0.07   -26%  1/1 1/1  TILTH ($)
gin_servehttp_flow                        $0.37   $0.29   -21%  1/1 1/1  TILTH ($)
express_json_send                         $0.26   $0.21   -20%  1/1 1/1  TILTH ($)
express_res_send                          $0.15   $0.12   -19%  1/1 1/1  TILTH ($)
gin_middleware_chain                      $0.49   $0.41   -16%  1/1 1/1  TILTH ($)
rg_flag_definition                        $0.11   $0.10   -15%  1/1 1/1  TILTH ($)
codebase_navigation                       $0.18   $0.16   -13%  1/1 1/1  TILTH ($)
rg_lineiter_definition                    $0.11   $0.10   -11%  1/1 1/1  TILTH ($)
─────────────────────────────────────────────────────────────────────────────────
express_render_chain                      $0.26   $0.25    -2%  1/1 1/1  ~tie
express_app_init                          $0.15   $0.15    +5%  1/1 1/1  ~tie
express_app_render                          inf     inf    ---  0/1 0/1  ~tie
─────────────────────────────────────────────────────────────────────────────────
markdown_section                          $0.06   $0.07   +14%  1/1 1/1  BASE ($)
gin_radix_tree                            $0.14   $0.16   +19%  1/1 1/1  BASE ($)
gin_context_next                          $0.05   $0.13  +140%  1/1 1/1  BASE ($)
rg_search_dispatch                        $0.56     inf     ↑∞  1/1 0/1  BASE (acc)
─────────────────────────────────────────────────────────────────────────────────
W19 T3 L4
```

Costs are $/correct (avg_cost / accuracy). Winner: accuracy difference > 15pp first, then >=10% cost difference.

### By language

| Repo | Language | $/correct (B → T) | Accuracy (B → T) |
|---|---|---|---|
| FastAPI | Python | $0.38 → $0.17 (-56%) | 100% → 100% |
| Express | JS | $0.24 → $0.23 (-5%) | 80% → 80% |
| Gin | Go | $0.29 → $0.24 (-15%) | 100% → 100% |
| ripgrep | Rust | $0.28 → $0.21 (-24%) | 100% → 83% |
| Synthetic | Multi | $0.11 → $0.09 (-22%) | 100% → 100% |

Python sees the largest improvement: cost per correct answer drops 56% with perfect accuracy. All languages improve. Only 2 failures: `express_app_render` (both modes fail — requires deep render chain tracing) and `rg_search_dispatch` (tilth only — intermittent on this complex Rust dispatch task).

## Opus 4.6 (10 runs)

5 hard tasks selected for Opus — tasks where Sonnet struggles or loses. 5 baseline + 5 tilth runs.

| | Baseline | tilth | Change |
|---|---|---|---|
| **Cost per correct answer** | **$0.29** | **$0.23** | **-22%** |
| Accuracy | 5/5 (100%) | 5/5 (100%) | 0pp |
| Avg cost per task | $0.29 | $0.23 | -22% |

```
Task                                     Base    Tilth   Delta  B✓  T✓
─────────────────────────────────────────────────────────────────────────
fastapi_depends_processing              $0.45   $0.21   -54%  1/1 1/1  TILTH ($)
rg_search_dispatch                      $0.67   $0.56   -18%  1/1 1/1  TILTH ($)
gin_context_next                        $0.06   $0.06   +10%  1/1 1/1  ~tie
express_app_render                      $0.13   $0.15   +12%  1/1 1/1  ~tie
gin_radix_tree                          $0.14   $0.16   +12%  1/1 1/1  ~tie
─────────────────────────────────────────────────────────────────────────
```

Opus achieves 100% accuracy in both modes and 100% tilth adoption. Notable: `express_app_render` and `rg_search_dispatch` — tasks Sonnet fails — are solved by Opus in both baseline and tilth modes.

## Haiku 4.5 (52 runs)

26 baseline + 26 tilth runs across all tasks.

| | Baseline | tilth | Change |
|---|---|---|---|
| **Cost per correct answer** | **$0.17** | **$0.19** | **+12%** |
| Accuracy | 15/26 (58%) | 18/26 (69%) | +12pp |
| Avg cost per task | $0.098 | $0.131 | +35% |
| Tilth adoption | — | 42% (94/228) | — |

tilth improves Haiku accuracy by 12pp (5 new tasks solved) but costs more per attempt (+35%). The net effect: +12% $/correct — the accuracy gain doesn't fully offset the cost increase.

W11 T11 L4. tilth wins include `rg_trait_implementors`, `rg_lineiter_usage`, `fastapi_depends_internals`, `gin_radix_tree`, and `gin_middleware_chain` — 5 tasks that baseline Haiku can't solve at all. Losses are mostly tasks where both modes fail but tilth spends more trying.

Haiku tilth adoption is only 42% — it defaults to Bash (102 calls) over tilth tools despite instruction tuning. 9 of 26 tilth runs used zero tilth tools (3 errored out, 6 used only host tools). Use `--disallowedTools "Bash,Grep,Glob"` to force adoption.

## Cross-model analysis

### Tool adoption by model (tilth mode)

| Model | tilth_search/run | tilth_read/run | tilth_files/run | Host tools/run | Adoption rate |
|---|---|---|---|---|---|
| Haiku 4.5 | 0.5 | 2.5 | 0.7 | 5.1 | 42% |
| Sonnet 4.5 | 2.5 | 3.4 | 1.2 | 0.1 | 98% |
| Opus 4.6 | 3.4 | 1.4 | — | 0 | 100% |

Adoption scales with model capability: Haiku 42%, Sonnet 98%, Opus 100%. Haiku heavily prefers Bash for code navigation despite instruction tuning — forced mode (`--disallowedTools`) remains recommended for smaller models.

### Where tilth wins

**fastapi_depends_function (-74% $/correct):** tilth's search results surface the function with full context and callees. Baseline takes 3x more tool calls to assemble the same picture.

**fastapi_depends_internals (-73%):** Similar pattern — tilth's callee footer resolves the dependency chain in a single search.

**rg_lineiter_usage (-69%):** tilth surfaces the usage sites efficiently with structural search. Baseline needs multiple grep/read cycles.

**Python overall (-56% $/correct):** All 5 FastAPI tasks improve with tilth. Perfect accuracy, cost drops across the board.

### Where tilth loses

**gin_context_next (+140%):** Baseline solves this cheaply ($0.05) while tilth explores more ($0.13). Both get correct answers — tilth just uses more tool calls.

**rg_search_dispatch (Sonnet tilth fails):** Complex Rust dispatch tracing. Intermittent — Sonnet previously solved this with tilth but failed on this run. Opus solves it consistently ($0.56, 100% tilth adoption).

**express_app_render (Sonnet fails both modes):** Deep render chain tracing. Opus ($0.15) and Haiku ($0.04) both solve it — Sonnet is the outlier here.

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
# All tasks, baseline + tilth, 3 reps, Sonnet
python benchmark/run.py --tasks all --repos ripgrep,fastapi,gin,express --models sonnet --reps 3

# Specific tasks
python benchmark/run.py --tasks fastapi_depends_processing,gin_middleware_chain --models sonnet --reps 3

# Opus on all tasks
python benchmark/run.py --tasks all --repos ripgrep,fastapi,gin,express --models opus --reps 3

# Haiku forced mode (built-in search tools removed)
python benchmark/run.py --tasks all --repos ripgrep,fastapi,gin,express --models haiku --reps 1 --modes tilth_forced

# Single mode only (skip baseline comparison)
python benchmark/run.py --tasks all --repos ripgrep,fastapi,gin,express --models sonnet --reps 1 --modes tilth
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
| v0.4.0 | def_weight ranking, basename boost, impl collector, sibling surfacing, transitive callees, faceted results, cognitive load stripping, smart truncation, symbol index, bloom filters | **-17%** (Sonnet), **-20%** (Opus) |
| v0.4.1 | Instruction tuning: "Replaces X" tool descriptions, explicit host tool naming in SERVER_INSTRUCTIONS | **-29%** (Sonnet), **-22%** (Opus) |

v0.4.1 focus: MCP instruction tuning. Tool descriptions now explicitly state which host tools they replace (e.g., "Replaces grep/rg and the host Grep tool"). SERVER_INSTRUCTIONS explicitly name host tools (Read, Grep, Glob) to replace. Result: tilth adoption jumped from 89% to 98%, and cost per correct answer improved from -17% to -29%.
