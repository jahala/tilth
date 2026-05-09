#!/usr/bin/env bash
# Regenerate AGENTS.md from prompts/mcp-base.md + prompts/mcp-edit.md.
# AGENTS.md is a generated artifact — edit the source files in prompts/, not AGENTS.md.
set -euo pipefail

cd "$(dirname "$0")/.."

base="prompts/mcp-base.md"
edit="prompts/mcp-edit.md"
out="AGENTS.md"

if [[ ! -f $base || ! -f $edit ]]; then
  echo "missing prompt source: $base or $edit" >&2
  exit 1
fi

{
  printf '<!-- generated from prompts/mcp-base.md + prompts/mcp-edit.md by scripts/regen-agents-md.sh — do not edit directly -->\n\n'
  cat "$base"
  printf '\n'
  cat "$edit"
} > "$out"

echo "wrote $out ($(wc -c < "$out") bytes)"
