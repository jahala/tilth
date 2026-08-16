---
status: trusted
last_verified: 2026-08-14
confidence: high
sources:
  - /home/paul/.local/share/cheese/paulnsorensen-tilth/specs/tilth-search-v2-roadmap.md
  - .cheese/research/tilth-api-analytics-verdict/report.md
  - https://github.com/paulnsorensen/tilth/issues/186
  - https://github.com/paulnsorensen/tilth/issues/187
---
# Independent Read and Write Adoption Lane

Read and write input cleanup proceeds in parallel with search v2, but under separate child specs, commits, benchmark variants, telemetry categories, and rollback decisions. Neither lane can block or authorize search-v2 graduation.

## Context

Analytics and open issues show repeated guessed read path objects and write fields. Running those changes in the same benchmark/version boundary as search v2 would make session-level error improvements impossible to attribute. Delaying all cleanup until after search graduation would preserve known friction unnecessarily.

## Decision

- Create an independently approved read child contract grounded in issue #186 and current telemetry.
- Create an independently approved write child contract grounded in issue #187 and current telemetry.
- Do not preselect exact aliases, accepted guessed fields, or normalization behavior in the search roadmap.
- Preserve write tags, seen-line gating, drift recovery, server-side safety checks, and section independence unless the write child contract explicitly re-ratifies a change.
- Give each lane separate commits, benchmark variants, telemetry categories, and rollback decisions.
- Keep search-v2 graduation independent from both lanes.

## Alternatives rejected

- **Serialize after search v2:** delays fixes with no technical dependency.
- **Ship before the v2 trial:** makes the baseline reflect a moving API surface.
- **Bundle with search v2:** confounds attribution and creates one oversized rollback boundary.
- **Specify aliases in this roadmap:** bypasses the required grounded approval cycle for each tool boundary.

## Consequences

The roadmap has two independent planning curds in its first wave. Their implementation behavior remains intentionally undecided until their own Mold handshakes. Telemetry can then distinguish search routing effects from read/write schema tolerance.
