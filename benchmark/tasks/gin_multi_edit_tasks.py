from tasks.base import Task, GroundTruth, Mutation


class GinMultiContextTask(Task):
    """Three independent regressions in context.go for batched same-file edits.

    The bugs sit in three unrelated methods (Next / Copy / reset) so the task
    measures simultaneous multi-site localization, not recall of the single-bug
    edit tasks. The prompt deliberately does not name the methods or the bug
    shapes — the agent localizes from the failing tests.
    """

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
            # Next(): pre-loop index advance, breaks the middleware chain.
            Mutation(
                file_path="context.go",
                original="c.index++",
                mutated="c.index += 2",
            ),
            # Copy(): detached-context index sentinel, so a goroutine copy can't
            # re-enter the handler chain. Novel site (not in any single-bug task).
            Mutation(
                file_path="context.go",
                original="cp.index = abortIndex",
                mutated="cp.index = 0",
            ),
            # reset(): pooled-context index initialization.
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
            "^(TestMiddlewareGeneralCase|TestContextCopy|TestContextReset)$",
            "-v",
        ]

    @property
    def prompt(self) -> str:
        return (
            "Three Gin context tests are failing after a batch of edits to context.go: "
            "TestMiddlewareGeneralCase, TestContextCopy, and TestContextReset. Each is a "
            "separate regression somewhere in context.go. Localize and fix all three so "
            "the tests pass, without changing unrelated behavior."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()
