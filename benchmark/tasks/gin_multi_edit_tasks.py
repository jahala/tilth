from tasks.base import Task, GroundTruth, Mutation


class GinMultiContextTask(Task):
    """Three independent regressions in context.go for batched same-file edits.

    The bugs sit in three unrelated methods (Next / Copy / reset). Two use novel
    shapes absent from every single-bug edit task — the Next() handler-loop bound
    flip and the Copy() detached-index sentinel — so the task can't be solved
    purely by recalling prior single-bug solutions. (reset()'s index-init mirrors
    gin_edit_context_reset; it is the only index mutation TestContextReset gates.)
    The prompt deliberately does not name the methods or bug shapes — the agent
    localizes all three from the failing tests.
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
            # Next(): flip the handler-loop bound (< -> >) so the chain never
            # advances past the first handler. Novel shape — gin_edit_middleware_skip
            # mutates `c.index++`, not the loop bound, so this can't be recalled.
            Mutation(
                file_path="context.go",
                original="c.index < safeInt8(len(c.handlers))",
                mutated="c.index > safeInt8(len(c.handlers))",
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
