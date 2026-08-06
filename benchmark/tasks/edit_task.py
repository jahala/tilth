from .base import Task, GroundTruth


class EditTask(Task):
    capability = "fix"
    @property
    def name(self) -> str:
        return "edit_task"

    @property
    def prompt(self) -> str:
        return (
            "In src/database/connection.py, change the return type annotation of the "
            "`get_pool` function to `Optional[ConnectionPool]`. Add the necessary import for Optional."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["Optional"],
            file_path="src/database/connection.py",
            expected_diff_contains=["Optional[ConnectionPool]", "Optional"]
        )

    @property
    def task_type(self) -> str:
        return "edit"
