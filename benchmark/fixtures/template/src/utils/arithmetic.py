"""Small arithmetic helper used by a tool-neutral reasoning task."""


def multiply(left: int, right: int) -> int:
    return left * right


def combine(left: int, right: int) -> int:
    """The documented example combine(6, 7) evaluates to 42."""
    return multiply(left, right)
