#!/usr/bin/env bash
# The landing page's garden footer names every bed of the garden, mirroring the
# umbrella site's list. The judge joins once the owner confirms its name.
set -euo pipefail
cd "$(dirname "$0")/../.."
beds=(tilth tend petals pleach umbel copeca pollen)
nav=$(awk '/<nav class="garden"/{p=1} p{print} /<\/nav>/{if(p)exit}' index.html)
[[ -n $nav ]] || { echo "✗ no <nav class=\"garden\"> in index.html" >&2; exit 1; }
status=0
for b in "${beds[@]}"; do
  grep -q "href=\"https://jahala.github.io/$b/\"" <<<"$nav" || { echo "✗ footer lacks $b" >&2; status=1; }
done
grep -q 'gf-plotbrand' index.html || { echo "✗ footer lacks the plotplot band" >&2; status=1; }
(( status == 0 )) && echo "footer lists all ${#beds[@]} beds and carries the plotplot band"
exit $status
