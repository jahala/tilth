# tilth Benchmark

Automated evaluation of tilth's impact on AI agent code navigation.

## Results — v0.4.1

| Model | Tasks | Runs | Baseline $/correct | tilth $/correct | Change | Baseline acc | tilth acc |
|---|---|---|---|---|---|---|---|
| Sonnet 4.5 | 26 | 52 | $0.26 | $0.19 | **-29%** | 96% | 92% |
| Opus 4.6 | 5 (tilth only) | 5 | — | $0.23 | — | — | 100% |
| Haiku 4.5 | 26 (tilth only) | 26 | — | $0.19 | — | — | 69% |

### Why "cost per correct answer"?

Raw cost comparison treats a wrong answer as a cheap success. It isn't — you paid for a response you can't use and still need the answer. The real question is: **how much do you expect to spend before you get a correct answer?**

This is a geometric retry model. If accuracy is `p`, you need `1/p` attempts on average before one succeeds. The expected cost is:

```
expected_cost = cost_per_attempt × (1 / accuracy)
```

**Cost per correct answer** (`total_spend / correct_answers`) computes this exactly. It's mathematically equivalent to `avg_cost / accuracy_rate` — not an arbitrary penalty, but the expected cost under retry.

## Sonnet 4.5 (52 runs)

26 tasks across 4 repos. 26 baseline + 26 tilth runs. 98% tilth tool adoption (185/188 tool calls used tilth).

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

## Opus 4.6 (31 shared runs)

6 tasks with both baseline and tilth data (18 baseline + 13 tilth). Opus was also tested on all 26 tasks (59 tilth runs total, 90% accuracy).

```
Task                                     Base    Tilth   Delta  B✓  T✓
─────────────────────────────────────────────────────────────────────────
fastapi_depends_processing              $0.41   $0.22   -47%   3/3 2/2  TILTH ($)
gin_middleware_chain                    $0.46   $0.28   -40%   3/3 2/2  TILTH ($)
rg_walker_parallel                        inf   $0.25    ↓∞    0/3 2/3  TILTH (acc)
gin_servehttp_flow                      $0.24   $0.31   +26%   3/3 2/2  BASE ($)
rg_search_dispatch                      $0.69   $1.00   +46%   3/3 1/2  BASE
fastapi_dependency_resolution           $0.41   $0.79   +91%   3/3 1/2  BASE (acc)
─────────────────────────────────────────────────────────────────────────
TOTAL                                                          15  10
```

| | Baseline | tilth | Change |
|---|---|---|---|
| **Cost per correct answer** | $0.49 | $0.39 | **-20%** |
| Accuracy | 83% (15/18) | 77% (10/13) | -6pp |
| Avg cost per task | $0.41 | $0.30 | -27% |

Opus uses tilth tools aggressively (1.8 tilth_search + 3.1 tilth_read per run). Across all 26 tasks (59 tilth runs), Opus achieves 90% accuracy (53/59 correct). The 6 failures are: `read_large_file` (0/4 — always misses `def rate_limit`), `rg_search_dispatch` (1/2), `fastapi_dependency_resolution` (1/2), `rg_walker_parallel` (2/3). Notable: `rg_walker_parallel` goes from 0/3 → 2/3, `rg_trait_implementors` is 6/6 (vs Sonnet's 3/7).

## Haiku 4.5 (26 tilth runs)

| | Haiku tilth |
|---|---|
| Accuracy | 18/26 (69%) |
| Avg cost | $0.131 |
| $/correct | $0.190 |
| Avg turns | 9.5 |
| Avg tool calls | 8.8 |
| Tilth adoption | 42% (96/228) |

Haiku is cheap enough ($0.19/correct) to be competitive despite lower accuracy (69%). It solves tasks that Sonnet fails — notably `express_app_render` ($0.04) and `gin_radix_tree` ($0.03). However, tilth adoption is only 42% — Haiku defaults to Bash (102 calls) over tilth tools despite instruction tuning. Use `--disallowedTools "Bash,Grep,Glob"` to force adoption.

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
| v0.4.1 | Instruction tuning: "Replaces X" tool descriptions, explicit host tool naming in SERVER_INSTRUCTIONS | **-29%** (Sonnet) |

v0.4.1 focus: MCP instruction tuning. Tool descriptions now explicitly state which host tools they replace (e.g., "Replaces grep/rg and the host Grep tool"). SERVER_INSTRUCTIONS explicitly name host tools (Read, Grep, Glob) to replace. Result: tilth adoption jumped from 89% to 98%, and cost per correct answer improved from -17% to -29%.
