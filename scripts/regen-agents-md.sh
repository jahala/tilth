#!/usr/bin/env bash
# Regenerate AGENTS.md from prompts/mcp-base.md + prompts/mcp-edit.md.
#
# AGENTS.md is a generated artifact — edit the source files in prompts/, not
# AGENTS.md. The contents are also embedded into the MCP server at compile time
# via include_str! in src/mcp/mod.rs; running this script keeps the human-facing
# AGENTS.md in lockstep with what MCP hosts receive in the `instructions` field.
#
# Idempotent: running twice produces no diff.
set -euo pipefail

cd "$(dirname "$0")/.."

base="prompts/mcp-base.md"
edit="prompts/mcp-edit.md"
out="AGENTS.md"

if [[ ! -f $base ]]; then
  echo "missing prompt source: $base" >&2
  exit 1
fi
if [[ ! -f $edit ]]; then
  echo "missing prompt source: $edit" >&2
  exit 1
fi

# Concatenate the two source files verbatim. mcp-edit.md starts with a leading
# blank-line pair to separate it visually from mcp-base.md in both the rendered
# AGENTS.md and the runtime instructions string (where format!("{S}{E}") relies
# on the same leading newlines).
{
  printf '<!-- generated from prompts/mcp-base.md + prompts/mcp-edit.md by scripts/regen-agents-md.sh — do not edit directly -->\n'
  cat "$base"
  cat "$edit"
  printf '\n'
} > "$out"

echo "wrote $out ($(wc -c < "$out" | tr -d ' ') bytes)"
