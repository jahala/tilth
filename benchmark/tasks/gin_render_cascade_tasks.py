from tasks.base import Task, GroundTruth, Mutation


class GinRenderContractCascadeTask(Task):
    """Cross-file contract cascade in gin's render package.

    Breaks the shared ``writeContentType`` helper's signature in render.go
    (``[]string`` -> ``string``) AND converts one caller's content-type var
    (xml.go) to match the new shape. The package no longer compiles: every
    other caller (json.go, text.go, yaml.go, toml.go, ...) still passes a
    ``[]string`` to the now-``string`` parameter.

    Unlike the independent-bug batch tasks, no file-local fix exists. Reverting
    the helper alone leaves xml.go broken; converting xml.go alone leaves the
    rest broken. The agent must trace every caller of ``writeContentType``
    across the package and reconcile them — exactly the cross-file callers /
    dependency reasoning that hard SWE-bench / CrossCodeEval instances target.
    """

    @property
    def name(self) -> str:
        return "gin_edit_render_cascade"

    @property
    def repo(self) -> str:
        return "gin"

    @property
    def task_type(self) -> str:
        return "edit"

    @property
    def mutations(self) -> list[Mutation]:
        return [
            # Root: change the shared helper's contract from []string to string.
            Mutation(
                file_path="render/render.go",
                original=(
                    "func writeContentType(w http.ResponseWriter, value []string) {\n"
                    "\theader := w.Header()\n"
                    "\tif val := header[\"Content-Type\"]; len(val) == 0 {\n"
                    "\t\theader[\"Content-Type\"] = value\n"
                    "\t}"
                ),
                mutated=(
                    "func writeContentType(w http.ResponseWriter, value string) {\n"
                    "\theader := w.Header()\n"
                    "\tif val := header[\"Content-Type\"]; len(val) == 0 {\n"
                    "\t\theader[\"Content-Type\"] = []string{value}\n"
                    "\t}"
                ),
            ),
            # One caller migrated to the new shape, so the break is genuinely
            # interdependent: reverting only the helper re-breaks this file.
            Mutation(
                file_path="render/xml.go",
                original='var xmlContentType = []string{"application/xml; charset=utf-8"}',
                mutated='var xmlContentType = "application/xml; charset=utf-8"',
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
            "The gin render package no longer builds. The shared helper "
            "`writeContentType` in render/render.go and its content-type "
            "callers across the package (json.go, text.go, xml.go, yaml.go, "
            "and others) have inconsistent types — some pass a string, others "
            "a []string. Restore the render package so it compiles and the "
            "render tests pass, keeping the emitted Content-Type headers "
            "unchanged. Make the helper and every caller agree on one shape."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()
