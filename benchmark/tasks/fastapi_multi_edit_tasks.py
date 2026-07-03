from tasks.base import Task, GroundTruth, Mutation


class FastAPIMultiResponseTask(Task):
    """Three regressions in serialize_response() in fastapi/routing.py."""

    @property
    def name(self) -> str:
        return "fastapi_edit_multi_response"

    @property
    def repo(self) -> str:
        return "fastapi"

    @property
    def task_type(self) -> str:
        return "edit"

    @property
    def mutations(self) -> list[Mutation]:
        return [
            Mutation(
                file_path="fastapi/routing.py",
                original="    if field:",
                mutated="    if not field:",
            ),
            Mutation(
                file_path="fastapi/routing.py",
                original="            by_alias=by_alias,",
                mutated="            by_alias=not by_alias,",
            ),
            Mutation(
                file_path="fastapi/routing.py",
                original="            exclude_none=exclude_none,",
                mutated="            exclude_none=not exclude_none,",
            ),
        ]

    @property
    def test_command(self) -> list[str]:
        return [
            "/usr/local/bin/python3.10",
            "-m",
            "pytest",
            "tests/test_response_model_data_filter.py::test_filter_second_level_model",
            "tests/test_response_by_alias.py::test_read_dict_by_alias",
            "tests/test_skip_defaults.py::test_return_exclude_none",
            "-x",
            "-q",
        ]

    @property
    def prompt(self) -> str:
        return (
            "Three focused FastAPI response-model tests are failing: "
            "test_filter_second_level_model, test_read_dict_by_alias, and "
            "test_return_exclude_none. All three regressions are in "
            "fastapi/routing.py, inside serialize_response(). Fix the broken "
            "response-model filtering, alias forwarding, and exclude-none "
            "forwarding without changing unrelated behavior."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()
