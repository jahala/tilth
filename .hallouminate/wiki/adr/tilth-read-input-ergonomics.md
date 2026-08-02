# ADRs — tilth_read input ergonomics (slug: tilth-read-input-ergonomics)

Session 2026-08-02; spec at `paulnsorensen-tilth/specs/tilth-read-input-ergonomics.md`.
Evidence: [[../usage-analytics-2026-07]].

### ADR-001: Bare-string paths coerces, and teaches batching [status: accepted]

- **Context:** 15 observed `paths must be an array of strings` errors, mostly
  scalar strings. A scalar → one-element array is unambiguous parsing (same
  category as the leading-# tag strip in
  [[tilth-write-teaching-errors]] ADR-003), but plain coercion would let
  agents settle into one-file-per-call reads.
- **Decision (user-amended):** Coerce AND append a batch-nudge note with an
  example (`paths: ["a.rs","b.rs"]`) so the call succeeds and the agent
  still learns the batch idiom. Non-string array elements keep erroring,
  now with the corrected shape.
- **Consequences:** Reverses the existing
  `tool_read_paths_wrong_type_reports_type_error` assertion — rewritten
  deliberately, approved at handshake.

### ADR-002: Unknown mode explains the edit-mode model [status: accepted]

- **Context:** Agents passed `mode: "edit"` expecting tagged reads.
  Grounded: `"edit"` was never a mode — tags mint automatically whenever
  the server runs with the edit_mode flag; `mode` only selects the view.
- **Decision:** The unknown-mode error names valid modes and states that
  tagged reads are automatic under the server's edit mode. No alias:
  accepting `mode:"edit"` as `auto` would confirm a false model of where
  tags come from.
