#!/usr/bin/env bash
# AGENTS.md is a no-op regeneration from prompts/ and keeps the tend2 block.
set -euo pipefail
cd "$(dirname "$0")/../.."
before=$(shasum -a 256 < AGENTS.md)
bash ./scripts/regen-agents-md.sh >/dev/null
after=$(shasum -a 256 < AGENTS.md)
[[ $before == "$after" ]] || { echo "✗ AGENTS.md was not a no-op regeneration — run scripts/regen-agents-md.sh and commit" >&2; exit 1; }
grep -q '<!-- tend2:begin -->' AGENTS.md && grep -q '<!-- tend2:end -->' AGENTS.md || { echo "✗ tend2 block missing from AGENTS.md" >&2; exit 1; }
echo "AGENTS.md regenerates as a no-op and carries the tend2 block"
