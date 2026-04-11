from tasks.base import Task, GroundTruth, Mutation


class GinDiffComprehensionTask(Task):
    """Understand a committed change without running tests. Pure diff comprehension."""

    @property
    def name(self) -> str:
        return "gin_diff_comprehension"

    @property
    def repo(self) -> str:
        return "gin"

    @property
    def task_type(self) -> str:
        return "read"

    @property
    def mutations(self) -> list[Mutation]:
        return [
            Mutation(
                file_path="gin.go",
                original='RemoteIPHeaders:        []string{"X-Forwarded-For", "X-Real-IP"},',
                mutated='RemoteIPHeaders:        []string{"X-Real-IP", "X-Forwarded-For"},',
            )
        ]

    @property
    def prompt(self) -> str:
        return (
            "The most recent commit changed how Gin handles client IP detection. "
            "Look at what changed in the last commit and explain: "
            "(1) which function or configuration was changed, "
            "(2) what the old behavior was, "
            "(3) what the new behavior is, and "
            "(4) what would break for users who rely on X-Forwarded-For having "
            "priority over X-Real-IP."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=[
                "RemoteIPHeaders",
                "X-Forwarded-For",
                "X-Real-IP",
            ],
        )
