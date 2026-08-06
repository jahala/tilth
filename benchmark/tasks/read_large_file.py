from .base import Task, GroundTruth


class ReadLargeFileTask(Task):
    capability = "locate"
    @property
    def name(self) -> str:
        return "read_large_file"

    @property
    def prompt(self) -> str:
        return "Show me the rate limiting logic in src/api/routes.py"

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["def rate_limit", "requests_per_minute", "@wraps"]
        )

    @property
    def task_type(self) -> str:
        return "read"
