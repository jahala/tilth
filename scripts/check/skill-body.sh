#!/usr/bin/env bash
# skills/SKILL.md carries the instructions the MCP server used to serve: its
# description is one sentence under 160 characters, and every non-empty line
# of the v0.10.1 SERVER_INSTRUCTIONS and EDIT_MODE_EXTRA appears verbatim in
# its body. The served block became a pointer; nothing an agent was told was
# dropped on the way.
set -euo pipefail
cd "$(dirname "$0")/../.."
skill=skills/SKILL.md
[[ -f $skill ]] || { echo "✗ $skill missing" >&2; exit 1; }
desc=$(awk 'BEGIN{fm=0} /^---$/{fm++; next} fm==1 && /^description:/{sub(/^description:[ ]*/,""); print; exit}' "$skill")
[[ -n $desc ]] || { echo "✗ no description in frontmatter" >&2; exit 1; }
len=${#desc}
sentences=$(printf '%s' "$desc" | grep -oE '[.!?]( |$)' | wc -l | tr -d ' ')
status=0
(( len < 160 )) || { echo "✗ description is $len characters (bar: under 160)" >&2; status=1; }
(( sentences == 1 )) || { echo "✗ description has $sentences sentence terminators, wants exactly one: $desc" >&2; status=1; }
body=$(awk 'BEGIN{fm=0} /^---$/{fm++; next} fm>=2' "$skill")
missing=0
for src in prompts/mcp-base.md prompts/mcp-edit.md; do
  while IFS= read -r line; do
    [[ -z ${line// /} ]] && continue
    if ! grep -qF -- "$line" <<<"$body"; then echo "✗ missing from $skill: $line" >&2; missing=$((missing+1)); fi
  done < <(git show "v0.10.1:$src")
done
(( missing == 0 )) || status=1
(( status == 0 )) && echo "SKILL.md: description $len chars, one sentence; every v0.10.1 instruction line present"
exit $status
