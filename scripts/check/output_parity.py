#!/usr/bin/env python3
"""Output parity: two tilth binaries print the same bytes on a fixed corpus.

Runs a base binary and a candidate binary over the benchmark fixture repos
(express, fastapi, gin, ripgrep) and compares what they print, case by case.
The corpus covers:

- the signature outline (`tilth_read` with `mode: "signature"`) of every
  source file each repo tracks, over MCP;
- the default CLI read of a sample of files;
- symbol search, callers, deps and grok on a sample of symbols and files,
  over both the CLI and MCP;
- a few whole-tree commands (map, overview, diff, glob) and error paths.

Each binary runs `--runs` times (default 3). A case whose base runs agree
with each other must match the candidate byte for byte (stdout, stderr and
exit code). A case whose base runs disagree is noise in the base binary
itself, so it is compared on a narrower view that the noise does not reach:
for symbol search, the definitions section; for anything else, its tokens
with order ignored. Every such case is named in the report.

The fixture repos are only read. The script records `git status` of each
repo before and after and fails if anything changed.

Usage:
  python3 scripts/check/output_parity.py --base OLD_TILTH --new NEW_TILTH \
      --repos benchmark/fixtures/repos [--work DIR] [--runs 3] [--compare-only]

Prints a markdown report on stdout. Exits 1 on any mismatch, 2 on a setup
error.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

REPOS = ("express", "fastapi", "gin", "ripgrep")

# Extensions tilth parses with a tree-sitter grammar, as `lang/mod.rs` maps them.
SOURCE_EXTS = {
    ".rs", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".py", ".go", ".java",
    ".scala", ".sc", ".c", ".h", ".cc", ".cpp", ".cxx", ".hpp", ".hh", ".hxx",
    ".rb", ".php", ".swift", ".kt", ".kts", ".cs", ".ex", ".exs", ".sh", ".bash",
}

# Per repo: files to read and analyse, symbols to search, callers and grok targets.
SAMPLES = {
    "express": {
        "files": ["lib/application.js", "lib/router/index.js", "lib/request.js", "index.js"],
        "symbols": ["createApplication", "send", "Router", "handle", "View"],
        "callers": ["handle", "send", "next"],
        "grok": ["createApplication", "View"],
        "glob": "*.js",
        "sub": "lib",
        "regex": "/function \\w+\\(req, res/",
    },
    "fastapi": {
        "files": ["fastapi/routing.py", "fastapi/params.py", "fastapi/dependencies/utils.py", "fastapi/applications.py"],
        "symbols": ["Depends", "APIRouter", "solve_dependencies", "FastAPI", "get_dependant"],
        "callers": ["solve_dependencies", "get_dependant", "Depends"],
        "grok": ["APIRoute", "solve_dependencies"],
        "glob": "*.py",
        "sub": "fastapi/dependencies",
        "regex": "/def \\w+_dependant/",
    },
    "gin": {
        "files": ["context.go", "gin.go", "tree.go", "binding/binding.go"],
        "symbols": ["Context", "Engine", "ServeHTTP", "handleHTTPRequest", "Next"],
        "callers": ["Abort", "Next", "handleHTTPRequest"],
        "grok": ["Next", "ServeHTTP"],
        "glob": "*.go",
        "sub": "binding",
        "regex": "/func \\(c \\*Context\\) \\w+/",
    },
    "ripgrep": {
        "files": ["crates/core/flags/defs.rs", "crates/core/main.rs", "crates/searcher/src/searcher/mod.rs", "crates/core/search.rs"],
        "symbols": ["Searcher", "Sink", "SearcherBuilder", "search_path", "Printer"],
        "callers": ["SearcherBuilder", "search_path", "new"],
        "grok": ["Searcher", "search_path"],
        "glob": "*.rs",
        "sub": "crates/core",
        "regex": "/fn \\w+_test/",
    },
}


def die(msg: str) -> None:
    print(f"setup error: {msg}", file=sys.stderr)
    sys.exit(2)


def git_status(repo: Path) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), "status", "--porcelain"],
        check=True, capture_output=True, text=True,
    ).stdout


def source_files(repo: Path) -> list[str]:
    out = subprocess.run(
        ["git", "-C", str(repo), "ls-files"], check=True, capture_output=True, text=True,
    ).stdout
    return sorted(
        f for f in out.splitlines()
        if Path(f).suffix in SOURCE_EXTS and (repo / f).is_file()
    )


def env() -> dict[str, str]:
    e = dict(os.environ)
    e["TILTH_THREADS"] = "1"
    e["PAGER"] = "cat"
    e.pop("TILTH_RESPECT_GITIGNORE", None)
    e.pop("TILTH_TIMEOUT", None)
    return e


def cli_cases(name: str, s: dict) -> list[tuple[str, list[str]]]:
    cases: list[tuple[str, list[str]]] = []
    for i, f in enumerate(s["files"]):
        cases.append((f"cli-read-{i}", [f]))
        cases.append((f"cli-deps-{i}", [f, "--deps"]))
    cases.append(("cli-read-section", [s["files"][0], "--section", "1-60"]))
    cases.append(("cli-read-budget", [s["files"][0], "--budget", "400"]))
    cases.append(("cli-read-markdown", ["README.md"]))
    for sym in s["symbols"]:
        cases.append((f"cli-search-{sym}", [sym, "--scope", "."]))
    cases.append((f"cli-search-expand-{s['symbols'][0]}", [s["symbols"][0], "--scope", ".", "--expand"]))
    cases.append((f"cli-search-glob-{s['symbols'][0]}", [s["symbols"][0], "--scope", ".", "--glob", s["glob"]]))
    cases.append(("cli-search-multi", [f"{s['symbols'][0]},{s['symbols'][1]}", "--scope", "."]))
    cases.append(("cli-search-content", ["TODO", "--scope", "."]))
    cases.append(("cli-search-regex", [s["regex"], "--scope", "."]))
    for sym in s["callers"]:
        cases.append((f"cli-callers-{sym}", [sym, "--scope", ".", "--callers"]))
    cases.append((f"cli-callers-expand-{s['callers'][0]}", [s["callers"][0], "--scope", ".", "--callers", "--expand"]))
    for sym in s["grok"]:
        cases.append((f"cli-grok-{sym}", ["grok", sym]))
    cases.append(("cli-glob", [s["glob"], "--scope", s["sub"]]))
    cases.append(("cli-map", ["--map", "--scope", s["sub"]]))
    cases.append(("cli-overview", ["overview"]))
    cases.append(("cli-diff", ["diff", "HEAD~1"]))
    cases.append(("cli-err-missing", ["no_such_file.xyz"]))
    return cases


def mcp_request(i: int, tool: str, args: dict) -> dict:
    return {"jsonrpc": "2.0", "id": i, "method": "tools/call", "params": {"name": tool, "arguments": args}}


INIT = {
    "jsonrpc": "2.0", "id": 0, "method": "initialize",
    "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "parity", "version": "0"}},
}


def mcp_sessions(root: str, files: list[str], s: dict) -> list[tuple[str, list[tuple[str, dict]], dict[str, str]]]:
    """Each session is (name, [(case, request)], extra env). Search-like calls
    get a session each, so the session's expanded-definition dedup in one call
    cannot leak into the next."""
    sessions = []
    reads = [(f"mcp-sig:{f}", mcp_request(i + 1, "tilth_read", {"path": f, "mode": "signature", "root": root}))
             for i, f in enumerate(files)]
    sessions.append(("mcp-signatures", reads, {"TILTH_NO_OVERVIEW": "1"}))

    single: list[tuple[str, str, dict]] = []
    for sym in s["symbols"]:
        single.append((f"mcp-search-{sym}", "tilth_search", {"query": sym, "root": root}))
    single.append((f"mcp-search-expand-{s['symbols'][0]}", "tilth_search", {"query": s["symbols"][0], "root": root, "expand": 3}))
    for sym in s["callers"]:
        single.append((f"mcp-callers-{sym}", "tilth_search", {"query": sym, "kind": "callers", "root": root}))
    for i, f in enumerate(s["files"]):
        single.append((f"mcp-deps-{i}", "tilth_deps", {"path": f, "root": root}))
        single.append((f"mcp-read-{i}", "tilth_read", {"path": f, "root": root}))
    for sym in s["grok"]:
        single.append((f"mcp-grok-{sym}", "tilth_grok", {"target": sym, "root": root}))
    single.append(("mcp-list", "tilth_list", {"patterns": [s["glob"]], "root": root}))
    single.append(("mcp-diff", "tilth_diff", {"source": "HEAD~1", "root": root}))
    for case, tool, args in single:
        sessions.append((case, [(case, mcp_request(1, tool, args))], {"TILTH_NO_OVERVIEW": "1"}))

    # One session with the fingerprint on, to cover `initialize` itself.
    sessions.append(("mcp-initialize", [], {}))
    return sessions


def run_cli(binary: str, repo: Path, args: list[str]) -> bytes:
    p = subprocess.run([binary, *args], cwd=repo, env=env(), capture_output=True, timeout=600)
    return b"".join([p.stdout, b"\n--stderr--\n", p.stderr, f"\n--exit {p.returncode}--\n".encode()])


def run_mcp(binary: str, repo: Path, requests: list[tuple[str, dict]], extra: dict[str, str]) -> dict[str, bytes]:
    lines = [json.dumps(INIT)] + [json.dumps(req) for _, req in requests]
    e = env()
    e.update(extra)
    p = subprocess.run([binary, "--mcp"], cwd=repo, env=e, input=("\n".join(lines) + "\n").encode(),
                       capture_output=True, timeout=1800)
    by_id: dict[int, bytes] = {}
    for line in p.stdout.splitlines():
        try:
            by_id[json.loads(line).get("id")] = line
        except json.JSONDecodeError:
            by_id.setdefault(-1, b"")
            by_id[-1] += line + b"\n"
    out: dict[str, bytes] = {}
    if not requests:
        out["mcp-initialize"] = by_id.get(0, b"<no initialize response>") + f"\n--exit {p.returncode}--\n".encode()
        return out
    for case, req in requests:
        out[case] = by_id.get(req["id"], b"<no response>") + f"\n--exit {p.returncode}--\n".encode()
    return out


def collect(binary: str, repos: Path, dest: Path) -> None:
    for name in REPOS:
        repo = repos / name
        root = str(repo.resolve())
        s = SAMPLES[name]
        files = source_files(repo)
        results: dict[str, bytes] = {}
        for case, args in cli_cases(name, s):
            results[case] = run_cli(binary, repo, args)
        for _session, requests, extra in mcp_sessions(root, files, s):
            results.update(run_mcp(binary, repo, requests, extra))
        out = dest / name
        out.mkdir(parents=True, exist_ok=True)
        (out / "results.json").write_text(json.dumps({k: v.decode("utf-8", "replace") for k, v in results.items()}))
        print(f"  {dest.name} · {name}: {len(results)} cases", file=sys.stderr)


def definitions_view(text: str) -> str:
    """The definitions section of a symbol search: from the first `## Definitions`
    heading up to the next `## ` heading. Usage counts live outside it."""
    if text.startswith("{"):
        try:
            text = json.loads(text.split("\n--exit")[0])["result"]["content"][0]["text"]
        except (json.JSONDecodeError, KeyError, IndexError, TypeError):
            return ""
    out, inside = [], False
    for line in text.split("\n"):
        if line.startswith("## "):
            inside = line.startswith("## Definitions")
        if inside:
            out.append(line)
    return "\n".join(out)


def sorted_tokens(text: str) -> str:
    """Order-free view: JSON `\\n`/`\\t` escapes and commas become spaces, each
    line's whitespace tokens are sorted, then the lines are sorted. What the
    base binary prints in hash-map order (fingerprint dirs, diff symbol lists)
    compares equal here; any added, lost or changed token still differs."""
    text = text.replace("\\n", "\n").replace("\\t", " ").replace(",", " ")
    return "\n".join(sorted(" ".join(sorted(line.split())) for line in text.split("\n")))


def digest(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()[:16]


def compare(work: Path, runs: int) -> int:
    labels_base = [f"base{i}" for i in range(1, runs + 1)]
    labels_new = [f"new{i}" for i in range(1, runs + 1)]
    total = identical = narrowed = mismatches = 0
    noisy_rows, bad_rows, per_repo = [], [], []
    for name in REPOS:
        load = lambda label: json.loads((work / label / name / "results.json").read_text())
        base = [load(lab) for lab in labels_base]
        new = [load(lab) for lab in labels_new]
        repo_total = repo_identical = repo_narrowed = repo_bad = 0
        sig_total = sig_identical = 0
        for case in base[0]:
            total += 1
            repo_total += 1
            b = [r.get(case) for r in base]
            n = [r.get(case) for r in new]
            if case.startswith("mcp-sig:"):
                sig_total += 1
            if all(x == b[0] for x in b):
                if all(x == b[0] for x in n):
                    identical += 1
                    repo_identical += 1
                    if case.startswith("mcp-sig:"):
                        sig_identical += 1
                else:
                    mismatches += 1
                    repo_bad += 1
                    bad_rows.append(f"| {name} | `{case}` | base stable | candidate differs |")
                continue
            # Base disagrees with itself: compare a view the noise does not reach.
            is_search = "-search-" in case
            view = definitions_view if is_search else sorted_tokens
            view_name = "definitions section" if is_search else "sorted tokens"
            views = {digest(view(x or "")) for x in b + n}
            if len(views) == 1 and (not is_search or definitions_view(b[0] or "")):
                narrowed += 1
                repo_narrowed += 1
                noisy_rows.append(f"| {name} | `{case}` | {view_name} | identical |")
            else:
                mismatches += 1
                repo_bad += 1
                bad_rows.append(f"| {name} | `{case}` | base noisy; {view_name} | candidate differs |")
        per_repo.append(f"| {name} | {repo_total} | {sig_total} ({sig_identical} identical) | {repo_identical} | {repo_narrowed} | {repo_bad} |")

    print("# Output parity\n")
    print(f"- base runs: {runs}, candidate runs: {runs}; TILTH_THREADS=1, PAGER=cat\n")
    print("| repo | cases | signature outlines | byte-identical | identical on a narrower view | mismatches |")
    print("|---|---|---|---|---|---|")
    print("\n".join(per_repo))
    print(f"| **all** | {total} | | {identical} | {narrowed} | {mismatches} |\n")
    if noisy_rows:
        print("Cases where the base binary disagreed with itself, compared on a narrower view:\n")
        print("| repo | case | view | verdict |\n|---|---|---|---|")
        print("\n".join(noisy_rows) + "\n")
    if bad_rows:
        print("Mismatches:\n")
        print("| repo | case | base | verdict |\n|---|---|---|---|")
        print("\n".join(bad_rows) + "\n")
    print(f"scratch: {work}")
    return 1 if mismatches else 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--base", required=True, help="base tilth binary")
    ap.add_argument("--new", required=True, help="candidate tilth binary")
    ap.add_argument("--repos", required=True, help="directory holding the fixture repos")
    ap.add_argument("--work", help="scratch directory (default: a new one under /tmp, kept for inspection)")
    ap.add_argument("--runs", type=int, default=3, help="runs per binary (default 3)")
    ap.add_argument("--compare-only", action="store_true", help="compare an existing --work directory")
    a = ap.parse_args()

    repos = Path(a.repos).resolve()
    for name in REPOS:
        if not (repos / name / ".git").exists():
            die(f"fixture repo {repos / name} missing")
    work = Path(a.work) if a.work else Path(tempfile.mkdtemp(prefix="tilth-parity-", dir="/tmp"))

    if not a.compare_only:
        for b in (a.base, a.new):
            if not os.access(b, os.X_OK):
                die(f"{b} is not an executable")
        before = {name: git_status(repos / name) for name in REPOS}
        for i in range(1, a.runs + 1):
            for label, binary in ((f"base{i}", a.base), (f"new{i}", a.new)):
                collect(str(Path(binary).resolve()), repos, work / label)
        after = {name: git_status(repos / name) for name in REPOS}
        if before != after:
            die("a run changed a fixture repo's git status")
    return compare(work, a.runs)


if __name__ == "__main__":
    sys.exit(main())
