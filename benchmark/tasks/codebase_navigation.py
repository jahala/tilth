from .base import Task, GroundTruth


class CodebaseNavigationTask(Task):
    capability = "locate"
    @property
    def name(self) -> str:
        return "codebase_navigation"

    @property
    def prompt(self) -> str:
        return (
            "What files in this codebase handle database operations? "
            "List each file with a one-line description of what it does."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["connection.py", "queries.py", "migrations.py"]
        )

    @property
    def task_type(self) -> str:
        return "navigate"
