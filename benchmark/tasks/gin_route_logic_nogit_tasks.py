from tasks.gin_route_logic_tasks import GinRouteCatchAllLogicTask


class GinRouteCatchAllNoGitTask(GinRouteCatchAllLogicTask):
    """No-git twin of ``gin_edit_route_catchall`` — same bug, no git oracle.

    Identical mutation, prompt, gate tests, and ground truth as the committed
    variant. The only difference: ``hide_git`` is True, so ``apply_mutations``
    writes the one-token ``tree.go`` flip *without* committing and moves ``.git``
    aside for the agent run. ``git log`` / ``git show`` / ``git diff`` /
    ``git blame`` all fail — the agent cannot read the isolated bug commit to
    localize the fix and must navigate the code instead.

    This isolates the git-history confound from Finding (c): comparing this task
    against ``gin_edit_route_catchall`` (same everything, git available) measures
    how much of baseline's success came from ``git show <bug-commit>`` rather
    than from actually reading the router/tree code. ``ensure_repo_clean``
    restores ``.git`` before the next reset.
    """

    @property
    def name(self) -> str:
        return "gin_edit_route_catchall_nogit"

    @property
    def hide_git(self) -> bool:
        return True
