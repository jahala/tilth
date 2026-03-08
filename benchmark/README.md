# tilth Benchmark

Automated evaluation of tilth's impact on AI agent code navigation.

## Results — v0.5.0

| Model | Tasks | Runs | Baseline $/correct | tilth $/correct | Change | Baseline acc | tilth acc |
|---|---|---|---|---|---|---|---|
| Sonnet 4.6 | 26 | 86 | $0.26 | $0.15 | **-44%** | 84% | 94% |
| Opus 4.6 | 26 | 25 | $0.22 | $0.14 | **-39%** | 91% | 92% |
| Haiku 4.5 | 26 | 49 | $0.12 | $0.08 | **-38%** | 54% | 73% |
| **Average** | | **160** | **$0.20** | **$0.12** | **-40%** | **76%** | **86%** |

### Why "cost per correct answer"?

Raw cost comparison treats a wrong answer as a cheap success. It isn't — you paid for a response you can't use and still need the answer. The real question is: **how much do you expect to spend before you get a correct answer?**

This is a geometric retry model. If accuracy is `p`, you need `1/p` attempts on average before one succeeds. The expected cost is:

```
expected_cost = cost_per_attempt × (1 / accuracy)
```

**Cost per correct answer** (`total_spend / correct_answers`) computes this exactly. It's mathematically equivalent to `avg_cost / accuracy_rate` — not an arbitrary penalty, but the expected cost under retry.

## Sonnet 4.6 (86 runs)

| | Baseline | tilth | Change |
|---|---|---|---|
| **Cost per correct answer** | **$0.26** | **$0.15** | **-44%** |
| Accuracy | 84% | 94% | +10pp |
| Avg cost per task | $0.23 | $0.14 | -40% |
| Avg turns | 9.0 | 6.2 | -31% |

v0.5.0 MCP instruction overhaul and scope fallback deliver -44% cost per correct answer with +10pp accuracy gain. Turn count drops 31% as models use tilth tools directly instead of falling back to built-in Grep/Read/Glob.

### Per-task results

```
Task                                       Base    Tilth   Delta  B✓  T✓  Winner
─────────────────────────────────────────────────────────────────────────────────
express_app_render                          inf   $0.17     ↓∞  0/1 2/2  TILTH (acc)
fastapi_depends_function                  $0.34   $0.08   -78%  1/1 2/2  TILTH ($)
rg_trait_implementors                     $0.29   $0.07   -75%  1/1 2/2  TILTH ($)
rg_lineiter_usage                         $0.30   $0.10   -68%  1/1 2/2  TILTH ($)
fastapi_depends_internals                 $0.31   $0.12   -60%  1/1 2/2  TILTH ($)
fastapi_depends_processing                $0.51   $0.21   -59%  1/1 2/2  TILTH ($)
find_definition                           $0.10   $0.05   -50%  1/1 2/2  TILTH ($)
gin_client_ip                             $0.38   $0.19   -50%  1/1 2/2  TILTH ($)
read_large_file                           $0.12   $0.07   -40%  1/1 2/2  TILTH ($)
express_res_send                          $0.15   $0.10   -36%  1/1 2/2  TILTH ($)
rg_walker_parallel                        $0.28   $0.18   -36%  1/1 2/2  TILTH ($)
edit_task                                 $0.09   $0.06   -30%  1/1 2/2  TILTH ($)
rg_flag_definition                        $0.11   $0.08   -26%  1/1 2/2  TILTH ($)
rg_search_dispatch                        $0.56   $0.42   -26%  1/1 2/2  TILTH ($)
codebase_navigation                       $0.18   $0.14   -25%  1/1 2/2  TILTH ($)
gin_middleware_chain                       $0.49   $0.39   -22%  1/1 2/2  TILTH ($)
fastapi_request_validation                $0.26   $0.21   -19%  1/1 2/2  TILTH ($)
fastapi_dependency_resolution             $0.45   $0.40   -12%  1/1 2/2  TILTH ($)
express_json_send                         $0.26   $0.23   -11%  1/1 2/2  TILTH ($)
─────────────────────────────────────────────────────────────────────────────────
gin_servehttp_flow                        $0.37   $0.34    -8%  1/1 2/2  ~tie
rg_lineiter_definition                    $0.11   $0.11    -3%  1/1 2/2  ~tie
express_render_chain                      $0.26   $0.25    -2%  1/1 2/2  ~tie
markdown_section                          $0.06   $0.06    +1%  1/1 2/2  ~tie
gin_radix_tree                            $0.14   $0.15    +9%  1/1 2/2  ~tie
─────────────────────────────────────────────────────────────────────────────────
express_app_init                          $0.15   $0.17   +14%  1/1 2/2  BASE ($)
gin_context_next                          $0.05   $0.10   +82%  1/1 2/2  BASE ($)
─────────────────────────────────────────────────────────────────────────────────
W19 T5 L2
```

Costs are $/correct (avg_cost / accuracy). Winner: accuracy difference > 15pp first, then >=10% cost difference.

### By language

| Repo | Language | $/correct (B → T) | Accuracy (B → T) |
|---|---|---|---|
| FastAPI | Python | $0.38 → $0.20 (-46%) | 100% → 100% |
| ripgrep | Rust | $0.28 → $0.16 (-42%) | 100% → 100% |
| Synthetic | Multi | $0.11 → $0.08 (-31%) | 100% → 100% |
| Express | JS | $0.24 → $0.18 (-23%) | 80% → 100% |
| Gin | Go | $0.29 → $0.23 (-19%) | 100% → 100% |

Rust sees the largest improvement (-42%) thanks to the read threshold bump — previously outlined files like ripgrep's `sink.rs` now return full content, eliminating multi-read spirals. All languages improve. `express_app_render` — previously unsolved by Sonnet — is solved in both tilth runs. `rg_search_dispatch` — previously intermittent — now succeeds 2/2.

## Opus 4.6 (25 runs)

| | Baseline | tilth | Change |
|---|---|---|---|
| **Cost per correct answer** | **$0.22** | **$0.14** | **-39%** |
| Accuracy | 91% | 92% | +1pp |
| Avg cost per task | $0.20 | $0.13 | -35% |
| Avg turns | 9.8 | 6.2 | -37% |

v0.5.0 delivers -39% cost per correct answer with 37% fewer turns. Opus already had high accuracy (91%); tilth maintains this while cutting costs significantly.

```
Task                                       Base    Tilth   Delta  B✓  T✓  Winner
─────────────────────────────────────────────────────────────────────────────────
fastapi_depends_internals                 $0.20   $0.09   -56%  1/1 1/1  TILTH ($)
rg_trait_implementors                     $0.16   $0.08   -50%  1/1 1/1  TILTH ($)
codebase_navigation                       $0.21   $0.11   -48%  1/1 1/1  TILTH ($)
fastapi_depends_function                  $0.11   $0.07   -41%  1/1 1/1  TILTH ($)
fastapi_depends_processing                $0.35   $0.22   -39%  1/1 1/1  TILTH ($)
find_definition                           $0.08   $0.05   -37%  1/1 1/1  TILTH ($)
rg_search_dispatch                        $0.66   $0.42   -37%  1/1 1/1  TILTH ($)
fastapi_dependency_resolution             $0.41   $0.26   -35%  1/1 1/1  TILTH ($)
gin_servehttp_flow                        $0.33   $0.22   -32%  1/1 1/1  TILTH ($)
gin_middleware_chain                      $0.33   $0.28   -16%  1/1 1/1  TILTH ($)
edit_task                                 $0.07   $0.06   -15%  1/1 1/1  TILTH ($)
markdown_section                          $0.06   $0.05   -15%  1/1 1/1  TILTH ($)
─────────────────────────────────────────────────────────────────────────────────
fastapi_request_validation                $0.19   $0.17    -8%  1/1 1/1  ~tie
express_json_send                         $0.23   $0.21    -8%  1/1 1/1  ~tie
rg_flag_definition                        $0.07   $0.06    -6%  1/1 1/1  ~tie
express_render_chain                      $0.26   $0.26    -0%  1/1 1/1  ~tie
read_large_file                             inf     inf    ---  0/1 0/1  ~tie
rg_lineiter_definition                    $0.06   $0.07    +3%  1/1 1/1  ~tie
gin_radix_tree                            $0.15   $0.15    +4%  1/1 1/1  ~tie
gin_client_ip                             $0.17   $0.18    +6%  1/1 1/1  ~tie
express_app_init                          $0.18   $0.20   +10%  1/1 1/1  ~tie
─────────────────────────────────────────────────────────────────────────────────
rg_walker_parallel                        $0.19   $0.22   +13%  1/1 1/1  BASE ($)
gin_context_next                          $0.05   $0.06   +20%  1/1 1/1  BASE ($)
express_app_render                        $0.14   $0.17   +25%  1/1 1/1  BASE ($)
rg_lineiter_usage                         $0.09   $0.12   +40%  1/1 1/1  BASE ($)
express_res_send                          $0.09   $0.12   +41%  1/1 1/1  BASE ($)
─────────────────────────────────────────────────────────────────────────────────
W12 T9 L5
```

## Haiku 4.5 (49 runs)

| | Baseline | tilth | Change |
|---|---|---|---|
| **Cost per correct answer** | **$0.12** | **$0.08** | **-38%** |
| Accuracy | 54% | 73% | +19pp |
| Avg cost per task | $0.10 | $0.06 | -40% |
| Avg turns | 10.4 | 9.7 | -7% |

v0.5.0 improves Haiku accuracy by 19pp and reduces cost per correct answer by 38%. The scope fallback fix prevents Haiku from passing invalid directory paths that caused 0-result searches and subsequent fallback to built-in tools.

## Cross-model analysis

### Tool adoption by model (tilth mode)

| Model | tilth_search/run | tilth_read/run | tilth_files/run | Host tools/run | Adoption rate |
|---|---|---|---|---|---|
| Haiku 4.5 | ~1.5 | ~4.0 | ~0.5 | ~1.0 | ~85% |
| Sonnet 4.6 | ~2.5 | ~3.0 | ~0.5 | ~0.2 | ~95% |
| Opus 4.6 | ~2.5 | ~3.0 | ~0.3 | ~0.1 | ~98% |

v0.5.0 MCP instructions with top-weighted DO NOT rules reduced host tool usage to near-zero on Sonnet and Opus. Haiku adoption improved significantly through scope fallback and instruction positioning.

### Where tilth wins

**fastapi_depends_function (-78% $/correct on Sonnet):** tilth's search results surface the function with full context and callees. Baseline takes 3x more tool calls to assemble the same picture.

**rg_trait_implementors (-75% Sonnet, -50% Opus):** Structural search finds all trait implementations efficiently. Baseline needs multiple grep/read cycles.

**fastapi_depends_internals (-60% Sonnet, -56% Opus):** tilth's callee footer resolves the dependency chain in a single search. Consistent wins across models.

**Rust overall (-42% $/correct on Sonnet):** The read threshold bump from ~3500 to ~6000 tokens eliminated multi-read spirals on mid-sized Rust files. `rg_search_dispatch` flipped from +90% regression to -26% win.

### Where tilth loses

**gin_context_next (+82% Sonnet, +20% Opus):** Simple struct field lookup. Baseline reads one file cheaply; tilth's search overhead isn't worthwhile on trivial tasks.

**express_app_render (+25% Opus):** Deep render chain tracing. Sonnet tilth solves this (2/2) where baseline can't, but Opus baseline is already efficient here.

**express_res_send (+41% Opus):** Short call chain where baseline's single Read is faster than tilth's search + read pattern.

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
| v0.4.4 | Adaptive 2nd-hop impact analysis for callers search, full 26-task Opus benchmark, Haiku adoption improvements | **-31%** (Sonnet), **-17%** (Opus), **-38%** (Haiku) |
| v0.4.5 | Read threshold bump from ~3500 to ~6000 tokens | **-34%** (Sonnet), **-19%** (Opus), **-38%** (Haiku) |
| v0.5.0 | MCP instruction overhaul (top-weighted DO NOT rules, search-first guidance), scope fallback with warning, Gemini CLI install | **-44%** (Sonnet), **-39%** (Opus), **-38%** (Haiku) |

v0.4.5 focus: bumped `TOKEN_THRESHOLD` from 3500 to 6000 estimated tokens (~24KB). Files in the 14–24KB range now return full content instead of an outline, eliminating multi-read spirals where agents read the entire file via 5–7 sequential `--section` calls. Key fixes: `gin_radix_tree` flipped from +35% regression to ~tie, `rg_search_dispatch` flipped from +90% regression to -26% win (Sonnet now 100% accuracy). Sonnet achieves 100% accuracy across all 52 tilth runs.
