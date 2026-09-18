#!/usr/bin/env bash
# The library introduces no dependency the v0.10.1 package did not already
# carry: the set of package names in Cargo.lock equals the set at tag v0.10.1,
# with the workspace's own new crate as the only permitted addition.
set -euo pipefail
cd "$(dirname "$0")/../.."
names() { awk '/^name = /{gsub(/"/,"",$3); print $3}' | sort -u; }
base=$(git show v0.10.1:Cargo.lock | names)
now=$(names < Cargo.lock)
added=$(comm -13 <(printf '%s\n' "$base") <(printf '%s\n' "$now") | grep -vx 'tilth-core' || true)
removed=$(comm -23 <(printf '%s\n' "$base") <(printf '%s\n' "$now") || true)
status=0
if [[ -n $added ]]; then echo "✗ packages added since v0.10.1:"; printf '  %s\n' $added; status=1; fi
if [[ -n $removed ]]; then echo "✗ packages removed since v0.10.1:"; printf '  %s\n' $removed; status=1; fi
(( status == 0 )) && echo "Cargo.lock package set unchanged from v0.10.1 ($(printf '%s\n' "$base" | wc -l | tr -d ' ') packages; tilth-core is the only permitted addition)"
exit $status
