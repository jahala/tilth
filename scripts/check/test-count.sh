#!/usr/bin/env bash
# The workspace runs at least as many tests as v0.10.1 did, all passing, none
# ignored. v0.10.1 ran 654: 650 in the library, 4 in the binary. A split that
# loses a test, or parks one behind #[ignore], fails here.
set -euo pipefail
cd "$(dirname "$0")/../.."
baseline=654
out=$(cargo test --workspace 2>&1) || { printf '%s\n' "$out" | tail -40; echo "✗ cargo test failed" >&2; exit 1; }
read -r passed failed ignored < <(printf '%s\n' "$out" | awk '/^test result/ {p+=$4; f+=$6; i+=$8} END {print p+0, f+0, i+0}')
echo "tests: $passed passed, $failed failed, $ignored ignored (baseline $baseline)"
status=0
(( passed >= baseline )) || { echo "✗ fewer tests than v0.10.1: $passed < $baseline" >&2; status=1; }
(( failed == 0 )) || { echo "✗ $failed failing" >&2; status=1; }
(( ignored == 0 )) || { echo "✗ $ignored ignored" >&2; status=1; }
exit $status
