from tasks.base import Task, GroundTruth


class FastAPIDependencyResolutionTask(Task):
    capability = "trace"
    @property
    def name(self) -> str:
        return "fastapi_dependency_resolution"

    @property
    def repo(self) -> str:
        return "fastapi"

    @property
    def prompt(self) -> str:
        return (
            "Trace how FastAPI resolves dependencies at request time. Starting from when "
            "a request hits an endpoint with Depends(), show the code path through "
            "dependency resolution. What function analyzes the callable's parameters, and "
            "what function actually executes the dependency chain?"
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["param_functions.py", "Depends", "dependency"],
        )

    @property
    def task_type(self) -> str:
        return "navigate"


class FastAPIRequestValidationTask(Task):
    capability = "trace"
    @property
    def name(self) -> str:
        return "fastapi_request_validation"

    @property
    def repo(self) -> str:
        return "fastapi"

    @property
    def prompt(self) -> str:
        return (
            "In FastAPI's routing.py, find the get_request_handler function. Show how it "
            "creates the actual request handler that processes incoming requests. Specifically, "
            "what happens when request validation fails? What exception is raised and where?"
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=[
                "get_request_handler",
                "solve_dependencies",
                "RequestValidationError",
                "async def app",
            ],
        )


class FastAPIDependsInternalsTask(Task):
    capability = "locate"
    @property
    def name(self) -> str:
        return "fastapi_depends_internals"

    @property
    def repo(self) -> str:
        return "fastapi"

    @property
    def prompt(self) -> str:
        return (
            "In fastapi/param_functions.py, find the Depends function. Show its complete "
            "signature including all parameters. Then explain: is Depends a class or a function? "
            "What does it actually return, and where is the underlying implementation?"
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["def Depends", "use_cache", "params.Depends"],
        )
