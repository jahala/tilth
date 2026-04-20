from tasks.base import Task, GroundTruth, Mutation


class RipgrepEditLineCountTask(Task):
    """Off-by-one in line count: adds 1 to every newline count."""

    @property
    def name(self) -> str:
        return "rg_edit_line_count"

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
            "There is a bug in ripgrep's line counting logic in "
            "crates/searcher/src/lines.rs. The count() function, which counts "
            "the number of line terminators in a byte slice, is consistently "
            "returning one more than the actual number of newlines. This causes "
            "line numbers in search output to be off by one. Find the arithmetic "
            "error in count() and fix it."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()


class RipgrepEditLineLocateTask(Task):
    """Off-by-one in line locate: returns newline position instead of line start."""

    @property
    def name(self) -> str:
        return "rg_edit_line_locate"

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
            "There is a bug in ripgrep's line location logic in "
            "crates/searcher/src/lines.rs. The locate() function, which finds "
            "the byte range of the line containing a given position, is returning "
            "a start offset that points to the newline terminator of the previous "
            "line instead of the first byte of the target line. This causes match "
            "highlighting to include the wrong leading character. Find the "
            "off-by-one error in the line start calculation and fix it."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()


class RipgrepEditPrecedingLinesTask(Task):
    """Boundary value change in preceding_by_pos: count == 0 becomes count == 1."""

    @property
    def name(self) -> str:
        return "rg_edit_preceding"

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
            "There is a bug in ripgrep's before-context logic in "
            "crates/searcher/src/lines.rs. The preceding_by_pos function, which "
            "finds the start of the Nth line before a given byte position, has "
            "a boundary error. When asked for 0 lines of context (i.e. the "
            "current line), it returns the wrong position — it's acting as if "
            "count=0 means 'go back one line' instead of 'stay on current line'. "
            "This breaks the -B 0 case and shifts all before-context by one line. "
            "Find the boundary check error in preceding_by_pos and fix it."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()
