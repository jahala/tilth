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

# Render both mode files, clearly labeled, so AGENTS.md documents the
# mode-select served instructions (one complete file per mode — no
# concatenation) rather than a single composed prompt.
{
  printf '<!-- generated from prompts/mcp-base.md + prompts/mcp-edit.md by scripts/regen-agents-md.sh — do not edit directly -->\n\n'
  printf '## Base mode\n\n'
  cat "$base"
  printf '\n\n## Edit mode\n\n'
  cat "$edit"
  printf '\n'
} > "$out"

echo "wrote $out ($(wc -c < "$out" | tr -d ' ') bytes)"
