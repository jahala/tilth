"""
Rename a Python identifier in source files using the tokenize module.
Only NAME tokens (identifiers) are renamed — string literals and comments
are left untouched.
"""
import tokenize
import io
import sys


def rename_identifier(filepath: str, old_name: str, new_name: str) -> None:
    with open(filepath, "r", encoding="utf-8") as f:
        source = f.read()

    # Collect (row, col) start/end for every NAME token matching old_name
    replacements: list[tuple[tuple[int, int], tuple[int, int]]] = []
    readline = io.StringIO(source).readline
    try:
        for tok_type, tok_string, tok_start, tok_end, _ in tokenize.generate_tokens(readline):
            if tok_type == tokenize.NAME and tok_string == old_name:
                replacements.append((tok_start, tok_end))
    except tokenize.TokenError as exc:
        print(f"TokenError in {filepath}: {exc}", file=sys.stderr)
        return

    # Build line→flat-offset mapping (1-indexed rows, 0-indexed cols)
    lines = source.splitlines(keepends=True)
    line_offsets: list[int] = []
    offset = 0
    for line in lines:
        line_offsets.append(offset)
        offset += len(line)

    def to_offset(row: int, col: int) -> int:
        return line_offsets[row - 1] + col

    # Convert to flat offsets and sort in reverse so we can splice from the end
    flat: list[tuple[int, int]] = []
    for (sr, sc), (er, ec) in replacements:
        flat.append((to_offset(sr, sc), to_offset(er, ec)))
    flat.sort(reverse=True)

    chars = list(source)
    for start, end in flat:
        chars[start:end] = list(new_name)

    new_source = "".join(chars)
    with open(filepath, "w", encoding="utf-8") as f:
        f.write(new_source)

    print(f"  {filepath}: renamed {len(flat)} occurrence(s)")


BASE = "/Users/flysikring/conductor/workspaces/tilth/almaty/benchmark/fixtures/repos/fastapi/fastapi"
OLD = "response_model_exclude_unset"
NEW = "exclude_unset_fields"

for filename in ("routing.py", "applications.py"):
    rename_identifier(f"{BASE}/{filename}", OLD, NEW)

print("Done.")
