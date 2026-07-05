# MCP cwd / workspace-root binding

How tilth resolves relative paths to the *right* checkout, why it uses a **required per-call `cwd` param** instead of the MCP `roots` capability, and the git-worktree gotcha that motivated it. Full cited research at `.cheese/research/mcp-cwd-root-binding/` and `.cheese/research/mcp-roots-trust-signal/` (local, gitignored). History: issue #78 / PR #79 (`root`, required-for-relatives) → **PR #113 `cwd-param-posture` (2026-07-04, current law)**.

## Current law (PR #113, spec `cwd-param-posture`)

- The param is named **`cwd`** (renamed from `root`) and is **schema-required on all seven path-taking tools** — `tilth_read`, `tilth_search`, `tilth_write`, `tilth_list`, `tilth_deps`, `tilth_grok`, `tilth_diff` (diff gained it for the first time; git-based sources still diff in the server's frozen dir, but the file-path params `patch`/`a`/`b` anchor relative spellings under `cwd` like every other tool).
- **Posture is trust-absolute**: relative paths anchor under `cwd` with `..` traversal refused (`Component::ParentDir`, leading and embedded); absolute paths pass through untouched — no confinement refusal. `resolve_confined`/`resolve_write_path`/`path_within_scope` were deleted; the shared helpers are `require_cwd` (teaching refusal on missing/relative `cwd`) and `resolve_anchored` in `src/mcp/tools/mod.rs`.
- **MCP roots support is gone entirely** — the one-shot post-initialize `roots/list` request and `extract_root_from_response` were deleted; the server never chdirs on client roots. (Rejected after round-5 adjudication; research at `.cheese/research/mcp-roots-trust-signal/`.)
- **Claude Code hook plugin** ships in-repo at `plugin/claude/`: a PreToolUse hook (`hooks/inject-cwd.js`, matcher `mcp__tilth__.*` — matchers are unanchored **regex**) injects the live session cwd via `hookSpecificOutput.updatedInput = {...tool_input, cwd}`; a model-set `cwd` wins. Field shape verified against <https://code.claude.com/docs/en/hooks.md> (merge-vs-replace is undocumented there; spreading the full input is correct under either).
- **`TILTH_MCP_CWD_HOOK_INJECTED`** env var flips the `cwd` schema description at definition time: `tilth install claude-code` writes `1` ("injected automatically, do NOT set"), codex and all other hosts write `0` ("always set explicitly"). When `1`, the server emits one stderr line at startup so an operator without the hook installed has something to grep — the server cannot observe actual hook presence, only the flag; a missing hook self-heals via the teaching error within a call or two.
- **Fork law** (CLAUDE.md): the `root`→`cwd` rename and trust-absolute posture are permanent fork patches with an accepted permanent sync-conflict surface; version stays 0.8.4.
- Haiku bench sanity on the hard tasks after the prompt rewrite: 8/9 new vs 8/9 old, identical per-task profile — required-`cwd` did not regress weak-model pass rates.

## The problem: a long-lived server can't learn the live cwd

tilth is a long-lived stdio MCP server, spawned once per session. It cannot *pull* the caller's current directory: the OS cwd of the tilth process is frozen at spawn; the MCP `tools/call` frame carries no cwd field; the directory the agent "is in" lives in another process or only in the agent's intent. So the live cwd must be *pushed* by the caller — an explicit argument on each call (or the Claude Code hook injecting it).

## The git-worktree gotcha (issue #78)

Server launched at `/repo` (main checkout); agent works in `/repo/.worktrees/<slug>/`. Relative reads/writes resolve against the **main checkout**, not the worktree. The read *succeeds* (reads the parent copy) so hash anchors match and the edit looks correct — the only tell is `git status` in the worktree coming up empty. Silent and dangerous. This is why omission is a hard teaching refusal, not a cwd fallback: a silent fallback is indistinguishable from a correct read until `git status` comes up empty.

## Why not the MCP `roots` capability

Cross-client research across 8 harnesses found **no client fires `notifications/roots/list_changed` for a git-worktree `cd`** — it's a terminal action, not a workspace-folder event. Adoption is patchy:

| Harness | roots | listChanged | Notes |
|---|---|---|---|
| VS Code (Copilot) | yes | yes (folder UI only) | not fired for a worktree `cd` |
| Claude Code | partial | no (#53861 open) | `roots/list` works v2.1.39+; `CLAUDE_PROJECT_DIR` set once at launch |
| Cursor | declares, broken | no | `roots/list` → `-32601 Method not found` |
| Zed / Codex / Cline / Continue / Windsurf | no | no | spawn-cwd at best, frozen at launch |

Per-call `cwd` is the only channel that fixes the worktree case on any client — matching the dominant peer-server pattern (`mcp-server-git` takes `repo_path` per call; the official `filesystem` server and DesktopCommander mandate per-call absolute paths). Round 5 went further than #79: even the one-shot startup `roots/list` was removed as a trust signal not worth keeping.

## Superseded designs (kept for archaeology)

- #78/#79 era: optional `root`, required only for relative paths, with confinement on writes (`resolve_confined`). Both halves replaced by PR #113.
- A stateful `set_default_root` tool — still rejected: invisible session state that can drift.
- Path-boundary sandboxing — rejected; trust-absolute is the adjudicated posture (B-prime cross-repo-only refuse was offered and declined, round 5).

## Related

- Issue <https://github.com/paulnsorensen/tilth/issues/78> · PR <https://github.com/paulnsorensen/tilth/pull/79> · **PR <https://github.com/paulnsorensen/tilth/pull/113>** (current)
- Spec: `cwd-param-posture` (durable corpus); decisions log in the spec is the adjudication record.
