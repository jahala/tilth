import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent))

from config import REPOS
from fixtures.setup import TEMPLATE_PATH
from tasks import TASKS
from tasks.base import TaskSource


VALID_CAPABILITIES = {"locate", "trace", "fix", "debug", "control"}
ADDED_TASKS = {
    "ts_express_middleware_locate",
    "ts_generic_trace",
    "java_locate_implementor",
    "fapi_deps_target",
    "gin_deps_target",
    "grok_rg_matcher",
    "control_changelog_read",
    "control_pkg_manifest",
    "control_arith_util",
}
BASE_TASKS = {
    "find_definition",
    "read_large_file",
    "edit_task",
    "codebase_navigation",
    "markdown_section",
    "rg_trait_implementors",
    "rg_flag_definition",
    "rg_search_dispatch",
    "rg_walker_parallel",
    "rg_lineiter_usage",
    "rg_edit_line_count",
    "rg_edit_line_locate",
    "rg_edit_preceding",
    "fastapi_edit_multi_response",
    "fastapi_dependency_resolution",
    "fastapi_request_validation",
    "fastapi_depends_internals",
    "fastapi_edit_dep_cache",
    "fastapi_edit_response_filter",
    "fastapi_edit_scope_cache",
    "gin_radix_tree",
    "gin_client_ip",
    "gin_middleware_chain",
    "gin_context_next",
    "gin_servehttp_flow",
    "gin_edit_middleware_skip",
    "gin_edit_abort_check",
    "gin_edit_context_reset",
    "gin_edit_multi_context",
    "gin_edit_render_cascade",
    "gin_edit_render_runtime",
    "gin_edit_route_catchall",
    "gin_edit_route_catchall_nogit",
    "express_json_send",
    "express_render_chain",
    "express_app_init",
    "express_res_send",
    "express_app_render",
    "express_edit_json_type",
    "express_edit_cookie_prefix",
    "express_edit_send_type",
    "express_diff_multi_mutation",
    "fastapi_diff_which_commit",
    "rg_diff_misdirected_error",
    "gin_diff_comprehension",
    "grok_gin_new",
    "grok_depends",
    "grok_context_next",
}
PRUNED_TASKS = {
    "rg_lineiter_definition",
    "fastapi_depends_function",
    "fastapi_depends_processing",
}


def test_task_registry_has_expected_corpus_shape() -> None:
    names = set(TASKS)
    assert names - BASE_TASKS == ADDED_TASKS
    assert names == BASE_TASKS | ADDED_TASKS
    assert len(TASKS) == 57
    assert len(names) == 57
    assert sum(bool(task.mutations) for task in TASKS.values()) == 22
    assert names.isdisjoint(PRUNED_TASKS)


def test_registered_tasks_have_explicit_capabilities_and_provenance() -> None:
    for task in TASKS.values():
        assert type(task).__dict__.get("capability") in VALID_CAPABILITIES
        assert task.capability in VALID_CAPABILITIES
        if task.repo == "synthetic":
            assert task.source == TaskSource()
        else:
            repo = REPOS[task.repo]
            assert task.source == TaskSource(
                origin=repo.url,
                license=repo.license,
                commit_or_tag=repo.commit_sha,
            )
    assert TASKS["markdown_section"].capability == "control"


def test_hardened_required_strings_are_discriminating() -> None:
    expected = {
        "express_render_chain": ["application.js", "app.render", "tryRender"],
        "express_res_send": ["response.js", "res.send", "Content-Length"],
        "gin_middleware_chain": [
            "gin.go",
            "Engine.ServeHTTP",
            "HandlersChain",
            "Context.Next",
        ],
        "gin_context_next": ["context.go", "Context.Next", "HandlersChain"],
        "grok_context_next": [
            "context.go",
            "Context.Next",
            "HandlersChain",
            "Context.Abort",
        ],
    }
    assert {
        name: TASKS[name].ground_truth.required_strings for name in expected
    } == expected
    generic_tokens = {"lookup", "view", "response", "pool", "index"}
    assert all(
        required.lower() not in generic_tokens
        for values in expected.values()
        for required in values
    )


def test_new_lookup_and_control_tasks_require_fixture_specific_facts() -> None:
    expected = {
        "fapi_deps_target": [
            "dependencies/models.py",
            "dependencies/utils.py",
            "ModelField",
            "RequiredParam",
        ],
        "gin_deps_target": [
            "github.com/gin-gonic/gin/internal/bytesconv",
            "BytesToString",
        ],
        "control_pkg_manifest": [
            "flask-api",
            ">=3.9",
            "flask>=2.3.0",
            "pyjwt>=2.8.0",
            "psycopg2-binary>=2.9.0",
            "pytest>=7.4.0",
        ],
        "gin_radix_tree": [
            "tree.go",
            "type node struct",
            "static",
            "root",
            "param",
            "catchAll",
            "getValue",
        ],
    }

    assert {
        name: TASKS[name].ground_truth.required_strings for name in expected
    } == expected


def test_history_comprehension_is_graded_from_the_response() -> None:
    task = TASKS["gin_diff_comprehension"]

    assert task.check_correctness(
        "RemoteIPHeaders changed X-Forwarded-For priority relative to X-Real-IP.",
        "/path/that/does/not/exist",
    ) == (True, "All checks passed")
    assert task.check_correctness(
        "The latest commit reordered two request headers.",
        "/path/that/does/not/exist",
    ) == (False, "Missing: RemoteIPHeaders")


def test_route_tasks_have_distinct_git_visibility() -> None:
    assert TASKS["gin_edit_route_catchall"].hide_git is False
    assert TASKS["gin_edit_route_catchall_nogit"].hide_git is True


def test_history_oracle_tasks_keep_git_visible() -> None:
    oracle_tasks = {
        "express_diff_multi_mutation",
        "fastapi_diff_which_commit",
        "rg_diff_misdirected_error",
        "gin_diff_comprehension",
    }

    assert {name: TASKS[name].hide_git for name in oracle_tasks} == {
        name: False for name in oracle_tasks
    }


def test_template_has_exact_corpus_fixture_mappings() -> None:
    fixtures = {
        path.relative_to(TEMPLATE_PATH).as_posix(): path.read_text(encoding="utf-8")
        for path in TEMPLATE_PATH.rglob("*")
        if path.is_file()
        and "__pycache__" not in path.parts
        and path.suffix not in {".pyc", ".pyo"}
    }
    expected_markers = {
        "src/typescript/express_middleware.ts": (
            "createAuthMiddleware",
            "app.use",
        ),
        "src/typescript/generic_trace.ts": (
            "GenericRepository",
            "findById",
            "loadUser",
        ),
        "src/java/Implementor.java": (
            "Notifier",
            "implements Notifier",
            "EmailNotifier",
            "WebhookNotifier",
        ),
    }
    obsolete = {
        "src/typescript/overloads.ts",
        "src/java/FactoryChain.java",
        "control/trace_origin.txt",
        "control/trace_destination.txt",
        "control/multistep.txt",
    }

    assert set(expected_markers) <= fixtures.keys()
    assert obsolete.isdisjoint(fixtures)
    for path, markers in expected_markers.items():
        assert all(marker in fixtures[path] for marker in markers)
    assert "Added" in fixtures["CHANGELOG.md"]
    assert "requires-python" in fixtures["pyproject.toml"]
    assert "42" in fixtures["src/utils/arithmetic.py"]
