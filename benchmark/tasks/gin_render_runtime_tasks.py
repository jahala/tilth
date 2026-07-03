from tasks.base import Task, GroundTruth, Mutation


class GinRenderRuntimeCascadeTask(Task):
    """Cross-file *runtime* regression across gin's render package.

    The runtime sibling of ``gin_edit_render_cascade``. Instead of breaking
    the build, it flips the Content-Type *values* of three renderers that each
    live in their own file (json.go, text.go, xml.go). The package compiles
    cleanly, so there are no compiler ``file:line`` breadcrumbs — the only
    signal is three failing test assertions (expected vs got Content-Type).

    The agent must localise each wrong value across three separate files from
    runtime test output alone, which is the navigation case where a structured
    symbol search (tilth) should beat error-list chasing. A YAML render test is
    included as a guard: it must stay green, so a fix that breaks the shared
    ``writeContentType`` helper to satisfy the others is rejected.
    """

    @property
    def name(self) -> str:
        return "gin_edit_render_runtime"

    @property
    def repo(self) -> str:
        return "gin"

    @property
    def task_type(self) -> str:
        return "edit"

    @property
    def mutations(self) -> list[Mutation]:
        return [
            # json.go: drop the charset so TestRenderJSON's exact-match fails.
            Mutation(
                file_path="render/json.go",
                original='[]string{"application/json; charset=utf-8"}',
                mutated='[]string{"application/json"}',
            ),
            # text.go: drop the charset so TestRenderString fails.
            Mutation(
                file_path="render/text.go",
                original='[]string{"text/plain; charset=utf-8"}',
                mutated='[]string{"text/plain"}',
            ),
            # xml.go: wrong media type so TestRenderXML fails.
            Mutation(
                file_path="render/xml.go",
                original='[]string{"application/xml; charset=utf-8"}',
                mutated='[]string{"text/xml; charset=utf-8"}',
            ),
        ]

    @property
    def test_command(self) -> list[str]:
        return [
            "go",
            "test",
            "-run",
            "^(TestRenderJSON|TestRenderXML|TestRenderString|TestRenderYAML)$",
            "./render/...",
            "-v",
        ]

    @property
    def prompt(self) -> str:
        return (
            "Several gin renderers are emitting the wrong Content-Type header. "
            "The failing tests are TestRenderJSON, TestRenderXML, and "
            "TestRenderString in the render package; TestRenderYAML still "
            "passes and must keep passing. The build is fine — each renderer "
            "defines its content type in its own file. Find where each render's "
            "Content-Type is set and restore the correct values so all four "
            "tests pass, without changing unrelated behavior."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()
