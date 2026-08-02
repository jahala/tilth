# ADRs — tilth_diff scope ergonomics (slug: tilth-diff-scope-ergonomics)

Session 2026-08-02; spec at the durable corpus
(`paulnsorensen-tilth/specs/tilth-diff-scope-ergonomics.md`). Evidence:
[[../usage-analytics-2026-07]] — directory-as-file diff failures observed in
all three harnesses (claude, codex, omp), including one omp agent burning six
consecutive calls on path variants of one directory.

### ADR-001: Directory prefix acts (filtered overview), not suggests [status: accepted]

- **Context:** The scope resolver matched overlays only exactly or by path
  suffix (`src/diff/mod.rs:302-321`), so a directory could never match. The
  repo has a settled suggest-only posture for *fuzzy* path resolution
  ([[../specs/fuzzy-suggest-only]]), raising the question whether a directory
  scope should also only suggest.
- **Decision:** A component-boundary directory prefix is an exact,
  unambiguous filter — not a fuzzy guess — so tilth_diff acts on it: render
  the overview restricted to files under the prefix, with a header naming
  the filter.
- **Alternatives:** Teaching error listing files under the prefix (zero
  behavior change, one extra round-trip per use — turns the observed 6-call
  thrash into 2 calls instead of 1); concatenated per-file detail (rejected:
  `scope: "src"` on a large diff blows the token budget immediately).
- **Consequences:** The suggest-only rule stays scoped to genuinely fuzzy
  matches (unmatched scopes get suggest-only did-you-mean, capped at 3, from
  the diff's own file list).

### ADR-002: Unresolvable refs teach, never auto-substitute [status: accepted]

- **Context:** Observed `git log failed: fatal: ambiguous argument
  'main..HEAD'` — the range is caller-supplied, so this is the agent
  guessing `main` in a repo whose default branch is something else. tilth
  could silently rewrite the base to the detected default branch.
- **Decision:** Teaching error only: append the repo's actual default branch
  (origin/HEAD symbolic-ref, fallback to local branch list) with a concrete
  `try '<branch>..HEAD'`. Auto-substitution acts on a guess — the agent may
  have meant a typo'd feature branch, and a silently substituted base
  produces a plausible-looking but wrong diff.
- **Consequences:** One extra round-trip on a bad ref, zero risk of wrong
  diffs; consistent with the fuzzy-suggest-only posture and the
  teaching-error idiom established in
  [[tilth-write-teaching-errors]] / tilth-write-json-ops ADR-003.
