from tasks.base import Task, GroundTruth, Mutation


class GinEditMiddlewareChainTask(Task):
    """Increment change in Next(): skips every other handler in the chain."""

    @property
    def name(self) -> str:
        return "gin_edit_middleware_skip"

    @property
    def repo(self) -> str:
        return "gin"

    @property
    def task_type(self) -> str:
        return "edit"

    @property
    def mutations(self) -> list[Mutation]:
        return [
            Mutation(
                file_path="context.go",
                original="c.index++",
                mutated="c.index += 2",
            )
        ]

    @property
    def test_command(self) -> list[str]:
        return ["go", "test", "-run", "TestMiddlewareGeneralCase", "-v"]

    @property
    def prompt(self) -> str:
        return (
            "In Gin's context.go, the middleware chain is broken — every other "
            "handler in the chain is being silently skipped. A route with "
            "middlewares A, C and handler D should execute as A→C→D→B (where B "
            "is A's post-Next code), but instead only alternating handlers run. "
            "Find the bug in the Next() method that causes handlers to be skipped "
            "and fix it."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()


class GinEditAbortCheckTask(Task):
    """Comparison change in IsAborted(): >= becomes >, breaks abort detection."""

    @property
    def name(self) -> str:
        return "gin_edit_abort_check"

    @property
    def repo(self) -> str:
        return "gin"

    @property
    def task_type(self) -> str:
        return "edit"

    @property
    def mutations(self) -> list[Mutation]:
        return [
            Mutation(
                file_path="context.go",
                original="c.index >= abortIndex",
                mutated="c.index > abortIndex",
            )
        ]

    @property
    def test_command(self) -> list[str]:
        return ["go", "test", "-run", "^TestContextIsAborted$", "-v"]

    @property
    def prompt(self) -> str:
        return (
            "In Gin's context.go, the IsAborted() method is broken — after "
            "calling Abort(), IsAborted() still returns false. This means "
            "middleware that checks IsAborted() after calling Abort() will "
            "incorrectly continue processing the request. The issue is a "
            "boundary comparison error in IsAborted(). Find and fix it."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()


class GinEditContextResetTask(Task):
    """Initial value change in reset(): index starts at 0 instead of -1."""

    @property
    def name(self) -> str:
        return "gin_edit_context_reset"

    @property
    def repo(self) -> str:
        return "gin"

    @property
    def task_type(self) -> str:
        return "edit"

    @property
    def mutations(self) -> list[Mutation]:
        return [
            Mutation(
                file_path="context.go",
                original="c.index = -1",
                mutated="c.index = 0",
            )
        ]

    @property
    def test_command(self) -> list[str]:
        return ["go", "test", "-run", "^TestContextReset$", "-v"]

    @property
    def prompt(self) -> str:
        return (
            "In Gin's context.go, the Context pool is producing incorrect "
            "behaviour after the first request. When a Context is returned to "
            "the sync.Pool and reused, the handler chain index is not properly "
            "reset — the first handler is being skipped on reused contexts. "
            "The issue is in the reset() method where the index is initialized "
            "to the wrong starting value. Find and fix it."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()
