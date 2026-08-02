# Diff: git ref resolution and exit-code handling

How `src/diff/` turns diff sources into git invocations, and the two traps that
have already bitten once each. Discovered while reviewing PR #147
(directory-prefix scoping); the fixes landed in commit `cce9f45`.

## The root commit cannot be diffed with `^..`

`diff_log` renders a per-commit summary by synthesizing one diff per commit in
the range. The obvious spelling is `{hash}^..{hash}` — and it works for every
commit except the first, which has no parent:

```
$ git diff a784574^..a784574
fatal: ambiguous argument 'a784574^..a784574': unknown revision or path not in
the working tree.
$ echo $?
128
```

This was latent for as long as `run_git_diff` returned `Ok(stdout)` on a
non-zero exit — the root commit simply rendered as an empty diff. PR #147 added
an exit-code guard, which turned the latent case into a hard failure of the
entire `log` mode for any range reaching the first commit. The only positive
log test used `HEAD~1..HEAD`, so CI stayed green.

**Use git's empty-tree object hash for parentless commits:**

```
4b825dc642cb6eb9a060e54bf8d69288fbee4904..{hash}
```

`diff_log` gets the parent list from `%P` in its `git log --format` string and
gates on it, so ordinary commits keep the `{hash}^..{hash}` spelling.

### `^!` is not the fix, despite looking like it

`git diff <hash>^!` is the canonical "show just this commit" spelling and is
the first thing anyone reaches for. It is wrong here, for two independent
reasons:

1. **For a root commit it silently means something else.** `^!` expands to the
   commit plus negated parents; with no parents there is nothing to negate, so
   it reduces to a *single* commit argument, and `git diff <commit>` means
   "this commit vs. the working tree." On tilth's own root commit that is 216
   files including deletions, where the correct answer (`git show --stat`) is
   34 files, additions only. It fails by returning plausible garbage, not by
   erroring.
2. **It breaks content resolution for every commit, not just the root.**
   `src/diff/overlay.rs`'s `get_old_content` and `resolve_git_ref_new_side`
   both split the ref on `..` to recover the two sides. A bare `^!` ref has no
   `..`, so old/new resolution silently degrades across the whole log.

Anything that changes the shape of a synthesized ref has to stay compatible
with that `..`-splitting contract.

## Exit codes: 0 and 1 are both success

`git diff` exits 0 when there is no diff and 1 when there is one; `--no-index`
follows the same convention. Anything else is fatal — commonly 128
(unresolvable revision) or 129 (not a repository).

The guard must cover **every** git-backed source, not just `GitRef`. When it
was gated on `GitRef` alone, running against a non-repo directory made
`git diff HEAD` exit 129 with empty stdout, and the caller was told
`"No changes."` — a clean tree reported for a command that never ran.

## Default-branch teaching hints

On an unresolvable ref, the error appends a detected default branch
(`try 'main..HEAD'`). Three constraints, each learned by getting it wrong:

- **Gate the hint on the failure cause.** Match git's stderr for
  `unknown revision` or `ambiguous argument`. Attached unconditionally, the
  hint tells someone whose `HEAD~50..HEAD` overran a 6-commit history to try
  `main..HEAD`, which does not address their problem.
- **Parse `symbolic-ref` output with `strip_prefix`, not `rsplit('/')`.**
  Branch names contain slashes: `refs/remotes/origin/release/stable` must yield
  `release/stable`, not `stable`. `rsplit` produces a confident hint pointing
  at a ref that does not exist.
- **Return `None` rather than guessing.** With no `origin/HEAD` and no local
  `main`/`master`, the earlier code fell back to the first branch `git branch`
  listed — i.e. the alphabetically first. In this repo that is
  `audit/core-budget`. A missing hint is strictly better than a wrong one
  asserted as fact.

## Related

- [Diff: symbol output order is not deterministic](diff-symbol-order-nondeterminism.md)
