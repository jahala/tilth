from tasks.base import Task, GroundTruth


class RipgrepTraitImplementorsTask(Task):
    @property
    def name(self) -> str:
        return "rg_trait_implementors"

    @property
    def repo(self) -> str:
        return "ripgrep"

    @property
    def prompt(self) -> str:
        return (
            "Find the `Matcher` trait definition in the matcher crate. "
            "Then find all types that implement this trait. For each implementor, "
            "show where it is defined and what crate it lives in."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["Matcher", "find_at", "RegexMatcher"],
        )

    @property
    def task_type(self) -> str:
        return "navigate"


class RipgrepFlagDefinitionTask(Task):
    @property
    def name(self) -> str:
        return "rg_flag_definition"

    @property
    def repo(self) -> str:
        return "ripgrep"

    @property
    def prompt(self) -> str:
        return (
            "In crates/core/flags/defs.rs, find the implementation of the --type-list flag. "
            "Show the complete Flag trait implementation for this flag, including all methods."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["TypeList", "impl Flag for", "name_long", "type-list"],
        )


class RipgrepSearchDispatchTask(Task):
    @property
    def name(self) -> str:
        return "rg_search_dispatch"

    @property
    def repo(self) -> str:
        return "ripgrep"

    @property
    def prompt(self) -> str:
        return (
            "Explain how ripgrep dispatches between single-line and multi-line search. "
            "Trace the code path from the Searcher to the actual matching logic. "
            "What structs are involved and how do the generic type parameters flow?"
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["ReadByLine", "MultiLine", "Sink", "glue.rs"],
        )

    @property
    def task_type(self) -> str:
        return "navigate"


class RipgrepWalkerParallelTask(Task):
    @property
    def name(self) -> str:
        return "rg_walker_parallel"

    @property
    def repo(self) -> str:
        return "ripgrep"

    @property
    def prompt(self) -> str:
        return (
            "In the ignore crate, find the parallel directory walker. Show the WalkParallel "
            "struct, the ParallelVisitor trait, and explain how work is distributed across threads."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["WalkParallel", "ParallelVisitor", "ParallelVisitorBuilder"],
        )

    @property
    def task_type(self) -> str:
        return "navigate"


class RipgrepLineIterDefinitionTask(Task):
    @property
    def name(self) -> str:
        return "rg_lineiter_definition"

    @property
    def repo(self) -> str:
        return "ripgrep"

    @property
    def prompt(self) -> str:
        return (
            "Find the LineIter struct definition in the ripgrep codebase. "
            "Show the struct and its fields, and explain what it's used for."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["LineIter", "bytes"],
        )


class RipgrepLineIterUsageTask(Task):
    @property
    def name(self) -> str:
        return "rg_lineiter_usage"

    @property
    def repo(self) -> str:
        return "ripgrep"

    @property
    def prompt(self) -> str:
        return (
            "Find the LineIter struct in ripgrep's searcher crate. Show its "
            "definition, then find where LineIter is constructed (look for "
            "LineIter::new or LineIter { calls). Show 2-3 key call sites."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=["LineIter", "lines.rs", "new"],
        )

    @property
    def task_type(self) -> str:
        return "navigate"
