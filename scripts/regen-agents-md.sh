#!/usr/bin/env bash
# Regenerate the human-facing AGENTS.md from the complete read-only and
# edit-mode prompt sources, labeled by mode.
#
# AGENTS.md is a generated artifact — edit the source files in prompts/, not
# AGENTS.md. The MCP server embeds both files via include_str! and selects one
# at runtime; AGENTS.md combines them only as a reference for disk-reading hosts.
#
# Idempotent: running twice produces no diff.
set -euo pipefail

cd "$(dirname "$0")/.."

base="prompts/mcp-base.md"
edit="prompts/mcp-edit.md"
out="AGENTS.md"

for f in "$base" "$edit"; do
  [[ -f $f ]] || { echo "missing prompt source: $f" >&2; exit 1; }
done

# Human-facing reference only. The MCP server selects one source file:
# mcp-base.md in read-only mode, mcp-edit.md in edit mode.
{
  printf '<!-- generated from prompts/mcp-base.md + prompts/mcp-edit.md by scripts/regen-agents-md.sh — do not edit directly -->\n\n'
  printf '## Base mode\n\n'
  cat "$base"
  printf '\n\n## Edit mode\n\n'
  cat "$edit"
  printf '\n'
} > "$out"

echo "wrote $out ($(wc -c < "$out" | tr -d ' ') bytes)"
