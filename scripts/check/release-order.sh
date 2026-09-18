#!/usr/bin/env bash
# The release workflow publishes tilth-core to crates.io before tilth: the
# binary depends on the library by version, and cargo refuses to publish a
# crate whose dependency is not on the registry. Also: the version check in
# the same workflow holds both crates to the tag.
set -euo pipefail
cd "$(dirname "$0")/../.."
wf=.github/workflows/release.yml
core_line=$(grep -n 'cargo publish --locked -p tilth-core' "$wf" | cut -d: -f1 | head -1)
bin_line=$(grep -n 'cargo publish --locked -p tilth$' "$wf" | cut -d: -f1 | head -1)
[[ -n $core_line && -n $bin_line ]] || { echo "✗ release.yml does not publish both crates" >&2; exit 1; }
(( core_line < bin_line )) || { echo "✗ release.yml publishes tilth (line $bin_line) before tilth-core (line $core_line)" >&2; exit 1; }
grep -q 'crates/tilth-core/Cargo.toml' "$wf" || { echo "✗ release.yml does not check tilth-core's version against the tag" >&2; exit 1; }
echo "release.yml publishes tilth-core (line $core_line) before tilth (line $bin_line) and checks both versions against the tag"
