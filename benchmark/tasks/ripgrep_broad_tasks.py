from tasks.base import Task, GroundTruth, Mutation


class RipgrepBroadLinecountTask(Task):
    """Broad prompt variant of rg_edit_line_count — no file/function hint."""

    @property
    def name(self) -> str:
        return "rg_broad_linecount"

    @property
    def repo(self) -> str:
        return "ripgrep"

    @property
    def task_type(self) -> str:
        return "edit"

    @property
    def mutations(self) -> list[Mutation]:
        return [
            Mutation(
                file_path="crates/searcher/src/lines.rs",
                original="memchr::memchr_iter(line_term, bytes).count() as u64",
                mutated="memchr::memchr_iter(line_term, bytes).count() as u64 + 1",
            )
        ]

    @property
    def test_command(self) -> list[str]:
        return ["cargo", "test", "-p", "grep-searcher", "line_count"]

    @property
    def prompt(self) -> str:
        return (
            "There's a regression in ripgrep — line numbers in search output are "
            "consistently off by one (always one too many). The issue is somewhere "
            "in the searcher crate. Run the tests, find the root cause, and fix it."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()


class RipgrepBroadLocateTask(Task):
    """Broad prompt variant of rg_edit_line_locate — no file/function hint."""

    @property
    def name(self) -> str:
        return "rg_broad_locate"

    @property
    def repo(self) -> str:
        return "ripgrep"

    @property
    def task_type(self) -> str:
        return "edit"

    @property
    def mutations(self) -> list[Mutation]:
        return [
            Mutation(
                file_path="crates/searcher/src/lines.rs",
                original="bytes[..range.start()].rfind_byte(line_term).map_or(0, |i| i + 1);",
                mutated="bytes[..range.start()].rfind_byte(line_term).map_or(0, |i| i);",
            )
        ]

    @property
    def test_command(self) -> list[str]:
        return ["cargo", "test", "-p", "grep-searcher", "line_locate_weird"]

    @property
    def prompt(self) -> str:
        return (
            "ripgrep's match highlighting includes a wrong leading character — the "
            "start position of matched lines seems to point to the newline of the "
            "previous line rather than the beginning of the target line. The issue "
            "is in the searcher crate. Find and fix the bug."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()


class RipgrepBroadPrecedingTask(Task):
    """Broad prompt variant of rg_edit_preceding — no file/function hint."""

    @property
    def name(self) -> str:
        return "rg_broad_preceding"

    @property
    def repo(self) -> str:
        return "ripgrep"

    @property
    def task_type(self) -> str:
        return "edit"

    @property
    def mutations(self) -> list[Mutation]:
        return [
            Mutation(
                file_path="crates/searcher/src/lines.rs",
                original="if count == 0 {",
                mutated="if count == 1 {",
            )
        ]

    @property
    def test_command(self) -> list[str]:
        return ["cargo", "test", "-p", "grep-searcher", "preceding_lines_doc"]

    @property
    def prompt(self) -> str:
        return (
            "ripgrep's before-context feature (-B flag) is showing one extra line "
            "of context than requested. When using -B 0 it still shows one line of "
            "before-context. The issue is in the searcher crate's line traversal "
            "logic. Find and fix it."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()
