from tasks.base import Task, GroundTruth, Mutation


class FastAPIEditDepCacheTask(Task):
    """Logical operator swap in dependency resolution: and → or breaks caching."""

    @property
    def name(self) -> str:
        return "fastapi_edit_dep_cache"

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
                file_path="fastapi/dependencies/utils.py",
                original="if sub_dependant.use_cache and sub_dependant.cache_key in dependency_cache:",
                mutated="if sub_dependant.use_cache or sub_dependant.cache_key in dependency_cache:",
            )
        ]

    @property
    def test_command(self) -> list[str]:
        return ["/usr/local/bin/python3.10", "-m", "pytest",
                "tests/test_dependency_cache.py::test_sub_counter", "-x", "-q"]

    @property
    def prompt(self) -> str:
        return (
            "In FastAPI's dependency injection system "
            "(fastapi/dependencies/utils.py), dependency caching is broken. "
            "When two route parameters both depend on the same dependency, the "
            "dependency should be resolved once per request and the cached result "
            "reused. Instead, it's hitting the cache even when caching is "
            "disabled (use_cache=False). The bug is in the cache lookup condition "
            "in the dependency resolution loop. Find the logic error and fix it."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()


class FastAPIEditResponseFilterTask(Task):
    """Condition negation in response serialization: skips filtering when it should apply."""

    @property
    def name(self) -> str:
        return "fastapi_edit_response_filter"

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
            )
        ]

    @property
    def test_command(self) -> list[str]:
        return ["/usr/local/bin/python3.10", "-m", "pytest",
                "tests/test_response_model_data_filter.py::test_filter_second_level_model",
                "-x", "-q"]

    @property
    def prompt(self) -> str:
        return (
            "In FastAPI's routing.py, the response model filtering is inverted. "
            "When an endpoint declares a response_model, the response should be "
            "serialized through that model to strip internal fields (like "
            "hashed_password). Instead, the serialization only runs when there "
            "is NO response model, and raw unfiltered data leaks through when "
            "a model IS declared. The bug is a negated condition in "
            "get_request_handler's response path. Find and fix it."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()


class FastAPIEditScopeCacheTask(Task):
    """Condition negation in security scope cache key: scoped/unscoped deps share keys."""

    @property
    def name(self) -> str:
        return "fastapi_edit_scope_cache"

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
                file_path="fastapi/dependencies/models.py",
                original="tuple(sorted(set(self.oauth_scopes or []))) if self._uses_scopes else ()",
                mutated="tuple(sorted(set(self.oauth_scopes or []))) if not self._uses_scopes else ()",
            )
        ]

    @property
    def test_command(self) -> list[str]:
        return ["/usr/local/bin/python3.10", "-m", "pytest",
                "tests/test_dependency_cache.py::test_security_cache", "-x", "-q"]

    @property
    def prompt(self) -> str:
        return (
            "In FastAPI's dependency models (fastapi/dependencies/models.py), "
            "Security dependencies with different OAuth scopes are incorrectly "
            "sharing cache keys. Security(dep, scopes=['read']) and "
            "Security(dep, scopes=['write']) should produce different cache keys "
            "so they're resolved separately, but they're being treated as the "
            "same dependency. The bug is in the cache_key property of the "
            "Dependant class — the scope inclusion logic is inverted. Find and "
            "fix it."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()
