from tasks.base import Task, GroundTruth, Mutation


class GinRouteCatchAllLogicTask(Task):
    """Cross-file *logic* regression in gin's radix-tree router.

    The correctness-discriminating sibling of the render cascade/runtime tasks.
    Where those hand the agent a greppable pointer to the root (a compiler
    file:line list, or an assertion diff printing the exact wrong string), this
    one hides the cause from text search entirely.

    A single one-token flip in ``tree.go`` changes how the catch-all (``*``)
    parameter key is sliced out of the node path: ``n.path[2:]`` (strip the
    leading ``/*``) becomes ``n.path[1:]`` (strip only ``/``), so the stored key
    for a ``/*wild`` route is ``*wild`` instead of ``wild``. The package
    compiles, routing still matches and returns 200, and ``:`` params are
    unaffected — only lookups of a catch-all param *by name* silently return
    empty.

    The failing tests are high-level router tests: ``PerformRequest`` →
    ``ServeHTTP`` → ``handleHTTPRequest`` → ``getValue``. Their output names a
    request *path* and prints ``expected "/is/super/great", actual ""`` — it
    never mentions ``tree.go``, ``getValue``, or the ``n.path`` slice. Grepping
    the failing value finds the route registration and the handler, not the
    tree math. Localising the cause requires walking the call chain from the
    router into the tree (tilth ``kind:callers`` / ``tilth_grok``), which is the
    navigation case a text search cannot express.

    Each gate test also asserts the sibling ``:name``/``:last_name`` params, so a
    fix that mangles the shared param-key extraction to satisfy the catch-all is
    rejected by the same tests.
    """

    @property
    def name(self) -> str:
        return "gin_edit_route_catchall"

    @property
    def repo(self) -> str:
        return "gin"

    @property
    def task_type(self) -> str:
        return "edit"

    @property
    def mutations(self) -> list[Mutation]:
        return [
            # tree.go getValue catchAll branch: strip only '/' instead of '/*',
            # so the catch-all param key keeps its leading '*' and by-name
            # lookups (c.Param / Params.Get) miss. Unique in tree.go — the param
            # branch uses n.path[1:] already, so this substring matches once.
            Mutation(
                file_path="tree.go",
                original="Key:   n.path[2:],",
                mutated="Key:   n.path[1:],",
            ),
        ]

    @property
    def test_command(self) -> list[str]:
        return [
            "go",
            "test",
            "-run",
            "^(TestRouteParamsByName|TestRouteParamsByNameWithExtraSlash|TestRouteParamsNotEmpty)$",
            ".",
            "-v",
        ]

    @property
    def prompt(self) -> str:
        return (
            "A gin router regression is breaking catch-all (wildcard) route "
            "parameters. The failing tests are TestRouteParamsByName, "
            "TestRouteParamsByNameWithExtraSlash, and TestRouteParamsNotEmpty "
            "in the gin package: a route like /test/:name/:last_name/*wild "
            "still returns 200, but looking up the *wild parameter by name "
            'returns "" instead of the matched path segment, so the tests fail '
            'on the wildcard value (expected "/is/super/great", got ""). The '
            "build is fine and the :name and :last_name params still work. Find "
            "the root cause and fix it so all three tests pass, without changing "
            "unrelated behavior."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()
