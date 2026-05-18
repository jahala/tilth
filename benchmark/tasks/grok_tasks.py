"""Tasks designed to exercise the tilth_grok tool.

Prompts are deliberately phrased as "understand this symbol" questions so the
agent can either (a) discover and use tilth_grok in one call, or (b) fall back
to the search → expand → search-callers chain. Cost-per-correct should drop
under (a) without sacrificing accuracy.

These tasks are NEW (no overlap with existing benchmarks) so before/after
comparisons aren't contaminated by tilth_search familiarity.
"""

from tasks.base import Task, GroundTruth


class GrokGinNewTask(Task):
    """Constructor with rich callers + internal callees: gin's `New()` engine factory.

    Replaces the original `grok_lineiter` task — LineIter was too small (4-line
    struct in a 30-line file) for grok to differentiate from a `grep + read`
    chain, and the ground truth didn't require the caller/callee detail that
    grok uniquely supplies. `New` has a 30-line body, 3 internal callees
    (`allocateContext`, `With`, `debugPrintWARNINGNew`), and 144 cross-file
    callers — all sections of grok's output are exercised."""

    @property
    def name(self) -> str:
        return "grok_gin_new"

    @property
    def repo(self) -> str:
        return "gin"

    @property
    def prompt(self) -> str:
        return (
            "Give me a comprehensive picture of gin's top-level `New()` "
            "constructor: what type it returns, the key fields it initializes "
            "on that type (defaults), the internal helper functions it calls, "
            "and approximately how many places in the codebase invoke it. "
            "One structured answer beats several searches."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        # required_strings forces the agent to surface info from EVERY grok
        # section: return type (Engine — body), an init field (RouterGroup —
        # body), an internal callee (allocateContext — callees), and the
        # canonical caller (Default — callers; also a sibling in gin.go).
        # We don't require the file name "gin.go" itself because an otherwise-
        # correct answer can legitimately describe the constructor without
        # reciting the file path.
        return GroundTruth(
            required_strings=[
                "Engine",
                "RouterGroup",
                "allocateContext",
                "Default",
            ],
        )

    @property
    def task_type(self) -> str:
        return "navigate"


class GrokDependsTask(Task):
    """Function with cross-file usages: FastAPI Depends + its processors."""

    @property
    def name(self) -> str:
        return "grok_depends"

    @property
    def repo(self) -> str:
        return "fastapi"

    @property
    def prompt(self) -> str:
        return (
            "Give me a complete picture of FastAPI's `Depends` function: its "
            "full signature, what it actually returns, the file it lives in, "
            "and which functions in the codebase call it directly. One "
            "structured answer is better than several partial ones."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["def Depends", "use_cache", "params.Depends"],
        )

    @property
    def task_type(self) -> str:
        return "navigate"


class GrokContextNextTask(Task):
    """Method on a struct: Gin's Context.Next + peer methods on Context."""

    @property
    def name(self) -> str:
        return "grok_context_next"

    @property
    def repo(self) -> str:
        return "gin"

    @property
    def prompt(self) -> str:
        return (
            "Show me Gin's `Context.Next` method: its implementation, the "
            "calls it makes inside, where it's invoked from, and the related "
            "methods on the same Context struct (Abort, Set, Get, etc.). "
            "I want one consolidated view, not piecemeal searches."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["Next", "index", "handlers", "Abort"],
        )

    @property
    def task_type(self) -> str:
        return "navigate"
