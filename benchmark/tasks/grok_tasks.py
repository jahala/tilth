"""Tasks designed to exercise the tilth_grok tool.

Prompts are deliberately phrased as "understand this symbol" questions so the
agent can either (a) discover and use tilth_grok in one call, or (b) fall back
to the search → expand → search-callers chain. Cost-per-correct should drop
under (a) without sacrificing accuracy.

These tasks are NEW (no overlap with existing benchmarks) so before/after
comparisons aren't contaminated by tilth_search familiarity.
"""

from tasks.base import Task, GroundTruth


class GrokLineIterTask(Task):
    """Single-symbol grok: LineIter struct (def + construction sites)."""

    @property
    def name(self) -> str:
        return "grok_lineiter"

    @property
    def repo(self) -> str:
        return "ripgrep"

    @property
    def prompt(self) -> str:
        return (
            "Tell me everything you can about the `LineIter` type in ripgrep — "
            "where it's defined, what fields it has, who constructs it, and "
            "what other types live alongside it in the same file. Aim for one "
            "comprehensive answer rather than fragmented searches."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["LineIter", "bytes", "lines.rs", "new"],
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
