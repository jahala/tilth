from tasks.base import Task, GroundTruth, Mutation


class RipgrepDiffMisdirectedErrorTask(Task):
    """Test error points at glue.rs but bug is in lines.rs. Diff reveals the real source."""

    @property
    def name(self) -> str:
        return "rg_diff_misdirected_error"

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
            "Integration tests in the searcher crate are failing — test output "
            "shows wrong line numbers in tests defined in glue.rs. The root "
            "cause may not be in the test file itself. A recent commit may have "
            "introduced this regression. Check what changed in the most recent "
            "commit and fix the bug."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth()
