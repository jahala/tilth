from tasks.base import Task, GroundTruth, Mutation


class GinMultiContextTask(Task):
    """Three regressions in context.go for batched same-file edits."""

    @property
    def name(self) -> str:
        return "gin_edit_multi_context"

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
            ),
            Mutation(
                file_path="context.go",
                original="c.index >= abortIndex",
                mutated="c.index > abortIndex",
            ),
            Mutation(
                file_path="context.go",
                original="c.index = -1",
                mutated="c.index = 0",
            ),
        ]

    @property
    def test_command(self) -> list[str]:
        return [
            "go",
            "test",
            "-run",
            "^(TestMiddlewareGeneralCase|TestContextIsAborted|TestContextReset)$",
            "-v",
        ]

    @property
    def prompt(self) -> str:
        return (
            "Three focused Gin context tests are failing: TestMiddlewareGeneralCase, "
            "TestContextIsAborted, and TestContextReset. All three regressions are in "
            "context.go. Fix the middleware-chain increment bug in Next(), the abort "
            "boundary bug in IsAborted(), and the reset index initialization bug in "
            "reset() without changing unrelated behavior."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()
