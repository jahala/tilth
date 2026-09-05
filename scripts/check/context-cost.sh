#!/usr/bin/env bash
# Measure tilth's startup context cost as an MCP server, the umbrella's probe:
# spawn the server, read `initialize.instructions` and `tools/list`, count
# characters, and convert at four characters per token.
#
# Runs in both modes (read-only, edit) and reports instructions, tool schemas,
# and the total. Exits 1 when the served instructions exceed the bar in either
# mode. The bar is the garden's: 120 tokens.
#
#   TILTH_BIN   binary to probe (default target/release/tilth)
#   PROBE_DIR   directory the server is launched in (default: this repo)
#   BAR         instructions token bar (default 120)
set -euo pipefail
cd "$(dirname "$0")/../.."

bin="${TILTH_BIN:-target/release/tilth}"
[[ -x $bin ]] || { echo "no binary at $bin — cargo build --release, or set TILTH_BIN" >&2; exit 2; }
bin="$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")"
probe_dir="${PROBE_DIR:-$PWD}"
bar="${BAR:-120}"

init='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"context-cost","version":"0"}}}'
list='{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'

tokens() { echo $(( ($1 + 3) / 4 )); }

status=0
printf '%-10s %14s %14s %14s\n' mode instructions tools total
for mode in read-only edit; do
  args=(--mcp)
  [[ $mode == edit ]] && args+=(--edit)
  out=$(printf '%s\n%s\n' "$init" "$list" | (cd "$probe_dir" && "$bin" "${args[@]}" 2>/dev/null))
  instr=$(jq -rs '.[] | select(.id==1) | .result.instructions // ""' <<<"$out")
  tools=$(jq -cs '.[] | select(.id==2) | .result.tools' <<<"$out")
  ic=${#instr}; tc=${#tools}
  it=$(tokens "$ic"); tt=$(tokens "$tc")
  printf '%-10s %6d ch %5d tk %6d ch %5d tk %6d ch %5d tk\n' "$mode" "$ic" "$it" "$tc" "$tt" $((ic+tc)) $((it+tt))
  if (( it > bar )); then
    echo "  ✗ $mode: instructions are $it tokens, bar is $bar" >&2
    status=1
  fi
done
echo "tools advertised: $(jq -r 'length' <<<"$tools") (edit mode)"
exit $status
