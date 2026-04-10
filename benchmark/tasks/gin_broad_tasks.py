from tasks.base import Task, GroundTruth, Mutation


class GinBroadMiddlewareTask(Task):
    """Broad variant of GinEditMiddlewareChainTask — prompt describes symptom, not file."""

    @property
    def name(self) -> str:
        return "gin_broad_middleware"

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
            "Gin's middleware chain is broken — handlers after the first middleware "
            "are being skipped. A route registered with multiple middlewares only "
            "executes alternating handlers; the ones in between are silently dropped. "
            "The request reaches the first middleware but never gets to subsequent "
            "handlers as expected. Find and fix the bug."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()


class GinBroadAbortTask(Task):
    """Broad variant of GinEditAbortCheckTask — prompt describes symptom, not file."""

    @property
    def name(self) -> str:
        return "gin_broad_abort"

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
            "Gin's abort mechanism isn't working — after calling c.Abort(), "
            "subsequent handlers in the chain still execute. The abort index check "
            "seems to be bypassed: IsAborted() returns false even when Abort() was "
            "called, so middleware that gates on IsAborted() lets requests through "
            "when they should be stopped. Find and fix the bug."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()
