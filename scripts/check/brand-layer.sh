#!/usr/bin/env bash
# .brand/products/tilth/ carries the product layer in the umbrella's shape:
# identity, colors, voice, and the mark, with tilth's one claimed accent.
set -euo pipefail
cd "$(dirname "$0")/../.."
d=.brand/products/tilth; status=0
for f in identity.md colors.md voice.md assets/logo.svg; do [[ -f $d/$f ]] || { echo "✗ missing $d/$f" >&2; status=1; }; done
(( status == 0 )) || exit 1
grep -q '#4E88A6' "$d/identity.md" && grep -q '#4E88A6' "$d/colors.md" || { echo "✗ tilth's accent Sky #4E88A6 must be named in identity.md and colors.md" >&2; status=1; }
for other in '#D6502F' '#E588A0' '#97539B' '#E89227' '#1F8A7B' '#C8B330'; do
  grep -q "$other" "$d/colors.md" && { echo "✗ another product's accent $other appears in colors.md" >&2; status=1; }
done
grep -q '| Product | tilth |' "$d/identity.md" || { echo "✗ identity.md must carry the Product row" >&2; status=1; }
grep -qi 'terminology' "$d/voice.md" || { echo "✗ voice.md must carry a terminology table" >&2; status=1; }
git ls-files --error-unmatch "$d" >/dev/null 2>&1 || { echo "✗ $d is not tracked (check .gitignore: .brand/* with !.brand/products/)" >&2; status=1; }
(( status == 0 )) && echo ".brand/products/tilth: identity, colors, voice, mark present; one accent, tracked"
exit $status
