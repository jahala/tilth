#!/usr/bin/env bash
# garden.json parses and declares what the fit contract reads: name, kind,
# version equal to both manifests (and the npm wrapper), faces, install per
# platform, metric command, and a declared context cost no lower than what a
# live server serves right now.
set -euo pipefail
cd "$(dirname "$0")/../.."
jq -e . garden.json >/dev/null
status=0
need() { jq -e "$1" garden.json >/dev/null 2>&1 || { echo "✗ garden.json: missing $1" >&2; status=1; }; }
need '.name == "tilth"'; need '.kind | length > 0'; need '.version'; need '.faces.cli'; need '.faces.mcp'; need '.faces.skill.path'
need '.install.cargo'; need '.install.npm'; need '.install.platforms | length > 0'; need '.metric.command'; need '.metric.latest'
need '.context_cost.instructions.read_only'; need '.context_cost.instructions.edit'; need '.context_cost.tool_schemas.read_only'; need '.context_cost.tool_schemas.edit'
v=$(jq -r .version garden.json)
for m in Cargo.toml crates/tilth-core/Cargo.toml; do
  mv=$(grep -m1 '^version' "$m" | sed 's/.*"\(.*\)"/\1/'); [[ $mv == "$v" ]] || { echo "✗ $m is $mv, garden.json says $v" >&2; status=1; }
done
nv=$(jq -r .version npm/package.json); [[ $nv == "$v" ]] || { echo "✗ npm/package.json is $nv, garden.json says $v" >&2; status=1; }
[[ -f $(jq -r .faces.skill.path garden.json) ]] || { echo "✗ skill path does not exist" >&2; status=1; }
[[ -f $(jq -r .metric.latest garden.json) ]] || { echo "✗ metric.latest does not exist" >&2; status=1; }
# declared ≥ measured: one measurement, the probe's, in the probe's unit
measured=$(bash scripts/check/context-cost.sh --json)
for mode in read_only edit; do
  it=$(jq -r ".$mode.instructions" <<<"$measured"); tt=$(jq -r ".$mode.tool_schemas" <<<"$measured")
  di=$(jq -r ".context_cost.instructions.$mode" garden.json); dt=$(jq -r ".context_cost.tool_schemas.$mode" garden.json)
  (( it <= di )) || { echo "✗ $mode instructions measure $it tokens, declared $di" >&2; status=1; }
  (( tt <= dt )) || { echo "✗ $mode tool schemas measure $tt tokens, declared $dt" >&2; status=1; }
  echo "$mode: instructions $it ≤ $di, tool schemas $tt ≤ $dt"
done
(( status == 0 )) && echo "garden.json: fields present, version $v in lockstep, declared context cost covers the measured"
exit $status
