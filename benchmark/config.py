import os
import shutil
from dataclasses import dataclass
from pathlib import Path
from typing import Optional


def resolve_tilth_bin() -> str:
    """Resolve the tilth binary portably so the benchmark runs on any machine.

    Order: ``$TILTH_BIN`` env override → ``tilth`` on ``PATH`` (the
    ``cargo install --path .`` workflow) → ``~/.cargo/bin/tilth`` fallback.
    """
    return (
        os.environ.get("TILTH_BIN")
        or shutil.which("tilth")
        or str(Path.home() / ".cargo" / "bin" / "tilth")
    )


TILTH_BIN = resolve_tilth_bin()

MODELS = {
    "haiku": "claude-haiku-4-5-20251001",
    "sonnet": "claude-sonnet-4-6",
    "opus": "claude-opus-5",
    "gpt5": "gpt-5.6-sol",
    "o3": "o3",
    # opencode + OpenRouter lane. Add more OpenRouter models here as one-liners
    # (short name -> "openrouter/<provider>/<model>"); mirror in RUNNERS.
    # gpt-5-mini: cheapest model with frontier-family tool-calling fidelity, so it
    # stays representative of real coding agents (the thing this tool-use A/B measures).
    "gpt5mini": "openrouter/openai/gpt-5-mini",
}

# Maps model short name -> runner type
RUNNERS = {
    "haiku": "claude",
    "sonnet": "claude",
    "opus": "claude",
    "gpt5": "codex",
    "o3": "codex",
    "gpt5mini": "opencode",
}

# MCP config arguments for codex (tilth server)
TILTH_MCP_CODEX_ARGS = [
    "-c", f'mcp_servers.tilth.command="{TILTH_BIN}"',
    "-c", 'mcp_servers.tilth.args=["--mcp", "--edit"]',
]


@dataclass
class ModeConfig:
    """Configuration for a benchmark mode (baseline vs tilth)."""
    name: str
    tools: list[str]
    mcp_config_path: Optional[str]
    description: str


REPO_ROOT = Path(__file__).parent.parent
BENCHMARK_DIR = Path(__file__).parent
FIXTURES_DIR = BENCHMARK_DIR / "fixtures"
SYNTHETIC_REPO = FIXTURES_DIR / "repo"
RESULTS_DIR = BENCHMARK_DIR / "results"
TILTH_MCP_CONFIG = FIXTURES_DIR / "tilth_mcp.json"

# opencode runner: per-mode opencode.json (OPENCODE_CONFIG env) selecting which
# MCP servers are exposed. opencode has no built-in-tool allowlist, so it covers
# baseline (no MCP) and tilth (tilth MCP); tilth_forced has no opencode analog.
OPENCODE_CONFIGS = {
    "baseline": str(FIXTURES_DIR / "opencode_baseline.json"),
    "tilth": str(FIXTURES_DIR / "opencode_tilth.json"),
}
REPOS_DIR = Path("/tmp/tilth_bench/repos")

# Hermetic XDG_CONFIG_HOME for OpenCode runs, so the user's global config,
# MCP servers, skills, and agents cannot merge into benchmark modes. The stable
# directory lets OpenCode reuse its bootstrapped plugin host between cells.
OPENCODE_CONFIG_HOME = REPOS_DIR.parent / "opencode_config_home"


@dataclass
class RepoConfig:
    """Configuration for a benchmark repository."""
    name: str
    url: str
    commit_sha: str
    language: str
    description: str
    license: str = "MIT"

    @property
    def path(self) -> Path:
        return REPOS_DIR / self.name


REPOS = {
    "ripgrep": RepoConfig(
        name="ripgrep",
        url="https://github.com/BurntSushi/ripgrep.git",
        commit_sha="0a88cccd5188074de96f54a4b6b44a63971ac157",
        language="rust",
        description="ripgrep line-oriented search tool",
        license="MIT OR Unlicense",
    ),
    "fastapi": RepoConfig(
        name="fastapi",
        url="https://github.com/tiangolo/fastapi.git",
        commit_sha="6fa573ce0bc16fe445f93db413d20146dd9ff35d",
        language="python",
        description="FastAPI web framework",
    ),
    "gin": RepoConfig(
        name="gin",
        url="https://github.com/gin-gonic/gin.git",
        commit_sha="d7776de7d444935ea4385999711bd6331a98fecb",
        language="go",
        description="Gin HTTP web framework",
    ),
    "express": RepoConfig(
        name="express",
        url="https://github.com/expressjs/express.git",
        commit_sha="1140301f6a0ed5a05bc1ef38d48294f75a49580c",
        language="javascript",
        description="Express.js web framework",
    ),
}

MODES = {
    "baseline": ModeConfig(
        name="baseline",
        tools=["Read", "Edit", "Grep", "Glob", "Bash"],
        mcp_config_path=None,
        description="Claude Code built-in tools",
    ),
    "tilth": ModeConfig(
        name="tilth",
        tools=["Read", "Edit", "Grep", "Glob", "Bash"],
        mcp_config_path=str(TILTH_MCP_CONFIG),
        description="Built-in tools + tilth MCP (hybrid)",
    ),
    "tilth_forced": ModeConfig(
        name="tilth_forced",
        tools=[],
        mcp_config_path=str(TILTH_MCP_CONFIG),
        description="tilth MCP only (no built-in tools)",
    ),
}

SYSTEM_PROMPT = """You are a code assistant. Answer the user's question about the codebase in the current directory.
Use the tools available to you to explore and understand the code.
Be precise and show relevant code when asked.
Any project guidance files (such as CLAUDE.md or AGENTS.md) do not apply to this task; rely only on the tools provided to you here and do not seek out or prefer tools those files mention."""

DEFAULT_REPS = 5
DEFAULT_MAX_BUDGET_USD = 1.0
