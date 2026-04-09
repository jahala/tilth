from tasks.base import Task, GroundTruth, Mutation


class ExpressDiffMultiMutationTask(Task):
    """Multi-mutation in one commit: 2 bugs + 1 harmless rename. Agent must diff to scope."""

    @property
    def name(self) -> str:
        return "express_diff_multi_mutation"

    @property
    def repo(self) -> str:
        return "express"

    @property
    def task_type(self) -> str:
        return "edit"

    @property
    def mutations(self) -> list[Mutation]:
        return [
            # Bug 1: json content type wrong
            Mutation(
                file_path="lib/response.js",
                original="this.set('Content-Type', 'application/json');",
                mutated="this.set('Content-Type', 'text/plain');",
            ),
            # Bug 2: cookie prefix wrong
            Mutation(
                file_path="lib/response.js",
                original="? 'j:' + JSON.stringify(value)",
                mutated="? 'x:' + JSON.stringify(value)",
            ),
            # Harmless: rename local variable (should NOT be reverted)
            Mutation(
                file_path="lib/response.js",
                original="var app = this.app;",
                mutated="var expressApp = this.app;",
            ),
        ]

    @property
    def test_command(self) -> list[str]:
        return ["npx", "mocha", "--require", "test/support/env", "--reporter",
                "spec", "--check-leaks", "--grep",
                "should respond with json for null|should generate a JSON cookie",
                "test/"]

    @property
    def prompt(self) -> str:
        return (
            "The last commit made several changes to lib/response.js — some "
            "refactoring and some behavior changes. Now tests are failing. "
            "Check what changed in the last commit, identify which changes are "
            "bugs vs intentional refactoring, and fix only the bugs. Do not "
            "revert the harmless refactoring changes."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()
