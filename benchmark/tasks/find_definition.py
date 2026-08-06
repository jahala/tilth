from .base import Task, GroundTruth


class FindDefinitionTask(Task):
    capability = "locate"
    @property
    def name(self) -> str:
        return "find_definition"

    @property
    def prompt(self) -> str:
        return "Find where `validate_jwt_token` is defined. Show the full implementation."

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["tokens.py", "def validate_jwt_token", "jwt.decode"]
        )

    @property
    def task_type(self) -> str:
        return "read"
