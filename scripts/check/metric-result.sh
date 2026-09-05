#!/usr/bin/env bash
# The declared metric's latest result is committed as a small summary: value,
# interval (or an honest "none"), date, version measured, pointer to the artifact.
set -euo pipefail
cd "$(dirname "$0")/../.."
f=$(jq -r .metric.latest garden.json)
[[ -f $f ]] || { echo "✗ $f missing" >&2; exit 1; }
git ls-files --error-unmatch "$f" >/dev/null 2>&1 || { echo "✗ $f is not tracked" >&2; exit 1; }
status=0
for k in 'Cost per correct' 'Confidence interval' 'Signed artifact' 'measured 20'; do
  grep -qi -- "$k" "$f" || { echo "✗ $f lacks: $k" >&2; status=1; }
done
grep -qE 'v[0-9]+\.[0-9]+\.[0-9]+' "$f" || { echo "✗ $f names no tilth version" >&2; status=1; }
(( status == 0 )) && echo "$f: value, interval, date, version, and artifact pointer present"
exit $status
