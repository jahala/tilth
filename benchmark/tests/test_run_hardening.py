"""Focused tests for runner isolation and result metadata."""

from dataclasses import dataclass
import io
import os
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent))

import run
from config import ModeConfig
from parse import RunResult, Turn
from tasks.base import TaskSource


_RUNTIME_KEYS = {
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "TERM",
    "LANG",
    "LANGUAGE",
    "XDG_DATA_HOME",
}
_AUTH_KEYS = {
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_CUSTOM_AUTH",
    "OPENAI_API_KEY",
    "OPENAI_ORG_ID",
    "OPENROUTER_API_KEY",
    "OPENROUTER_SITE_URL",
    "CODEX_API_KEY",
}


@pytest.mark.parametrize(
    ("runner", "opencode_config", "bare"),
    [
        ("claude", None, False),
        ("codex", None, False),
        ("opencode", "/controlled/opencode.json", False),
        ("opencode", "/controlled/opencode.json", True),
    ],
)
def test_build_runner_env_allowlists_ambient_environment(
    monkeypatch: pytest.MonkeyPatch,
    runner: str,
    opencode_config: str | None,
    bare: bool,
) -> None:
    """Each lane gets runtime/auth values but no unrelated host secret."""
    values = {key: f"value-for-{key}" for key in _RUNTIME_KEYS | _AUTH_KEYS}
    values["PATH"] = "/usr/bin"
    values["LC_CUSTOM"] = "custom-locale"
    for key, value in values.items():
        monkeypatch.setenv(key, value)

    monkeypatch.setenv("SENTINEL_SECRET", "do-not-forward")
    monkeypatch.setenv("CLAUDECODE", "nested-session")
    monkeypatch.setenv("CLAUDE_CODE_OAUTH_TOKEN", "do-not-forward")
    monkeypatch.setenv("CLAUDE_SESSION_ID", "do-not-forward")
    monkeypatch.setenv("MCP_SERVER_SECRET", "do-not-forward")
    monkeypatch.setenv("XDG_CONFIG_HOME", "/ambient/config")
    monkeypatch.setenv("OPENCODE_CONFIG", "/ambient/opencode.json")
    monkeypatch.setattr(run, "TILTH_BIN", "/opt/tilth/bin/tilth")

    ambient = dict(os.environ)
    expected = {
        key: value
        for key, value in ambient.items()
        if key in _RUNTIME_KEYS
        or key == "CODEX_API_KEY"
        or key.startswith(("ANTHROPIC_", "OPENAI_", "OPENROUTER_", "LC_"))
    }
    expected["PATH"] = f"/opt/tilth/bin{os.pathsep}{ambient['PATH']}"
    if runner == "opencode":
        expected["OPENCODE_CONFIG"] = opencode_config
        expected["XDG_CONFIG_HOME"] = str(run.OPENCODE_CONFIG_HOME)
    if bare:
        expected.update(
            {
                "OPENCODE_DISABLE_DEFAULT_PLUGINS": "1",
                "OPENCODE_DISABLE_PROJECT_CONFIG": "1",
                "OPENCODE_DISABLE_CLAUDE_CODE": "1",
                "OPENCODE_DISABLE_EXTERNAL_SKILLS": "1",
            }
        )

    env = run.build_runner_env(
        runner,
        opencode_config=opencode_config,
        bare=bare,
    )

    assert env == expected
    assert "SENTINEL_SECRET" not in env
    assert "CLAUDECODE" not in env
    assert "MCP_SERVER_SECRET" not in env
    assert not any(key.startswith("CLAUDE_") for key in env)
    if runner != "opencode":
        assert "OPENCODE_CONFIG" not in env
        assert "XDG_CONFIG_HOME" not in env


def test_agent_repo_export_excludes_repository_metadata(tmp_path: Path) -> None:
    source = tmp_path / "source"
    source.mkdir()
    (source / ".git").mkdir()
    (source / ".git_hidden").mkdir()
    tracked = source / "tracked.py"
    tracked.write_text("mutated")

    with run._agent_repo(source, hide_git=True) as workspace:
        assert workspace != source
        assert (workspace / "tracked.py").read_text() == "mutated"
        assert not (workspace / ".git").exists()
        assert not (workspace / ".git_hidden").exists()
        (workspace / "tracked.py").write_text("agent fix")

    assert tracked.read_text() == "mutated"


@dataclass
class _RunnerTask:
    repo: str = "synthetic"
    prompt: str = "Answer the task."
    capability: str = "trace"
    source: TaskSource = TaskSource(
        origin="fixture",
        license="MIT",
        commit_or_tag="test-pin",
        transformation="test-only",
    )

    def check_correctness(self, result_text: str, repo_path: str) -> tuple[bool, str]:
        return True, "expected answer"


@pytest.mark.parametrize(
    ("alias", "runner", "model_id", "parser_name"),
    [
        ("haiku", "claude", "configured-claude-model", "parse_stream_json"),
        ("gpt5", "codex", "configured-codex-model", "parse_codex_json"),
        ("gpt5mini", "opencode", "configured-opencode-model", "parse_opencode_json"),
    ],
)
def test_run_single_uses_allowlisted_env_and_preserves_runner_flags(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    alias: str,
    runner: str,
    model_id: str,
    parser_name: str,
) -> None:
    task = _RunnerTask()
    monkeypatch.setitem(run.TASKS, "runner_test_task", task)
    monkeypatch.setitem(run.MODELS, alias, model_id)
    monkeypatch.setitem(run.RUNNERS, alias, runner)
    monkeypatch.setitem(
        run.MODES,
        "runner_test_mode",
        ModeConfig(
            name="runner_test_mode",
            tools=["Read", "Edit"],
            mcp_config_path="/controlled/tilth_mcp.json",
            description="test mode",
        ),
    )
    monkeypatch.setitem(run.OPENCODE_CONFIGS, "runner_test_mode", "/controlled/opencode.json")
    monkeypatch.setattr(run, "get_repo_path", lambda _: tmp_path)
    monkeypatch.setattr(run, "_tilth_version", lambda: "test-version")
    monkeypatch.setenv("SENTINEL_SECRET", "do-not-forward")
    monkeypatch.setenv("PATH", "/usr/bin")
    monkeypatch.setattr(run, "TILTH_BIN", "/opt/tilth/bin/tilth")

    parsed = RunResult(
        session_id="session",
        turns=[
            Turn(
                index=0,
                input_tokens=3,
                output_tokens=17,
                cache_creation_tokens=2,
                cache_read_tokens=1,
                cache_creation_5m_tokens=1,
                cache_creation_1h_tokens=1,
            ),
        ],
        num_turns=1,
        total_cost_usd=0.01,
        duration_ms=12,
        duration_api_ms=10,
        total_input_tokens=3,
        total_output_tokens=17,
        total_cache_creation_tokens=2,
        total_cache_read_tokens=1,
        result_text="correct answer",
    )
    parser_calls: list[tuple] = []

    def fake_parser(*args):
        parser_calls.append(args)
        return parsed

    monkeypatch.setattr(run, parser_name, fake_parser)
    captured: dict[str, object] = {}

    def fake_subprocess_run(cmd, **kwargs):
        captured["cmd"] = cmd
        captured["env"] = kwargs["env"]
        captured["stdin"] = kwargs.get("stdin")
        return run.subprocess.CompletedProcess(cmd, 0, stdout="{}", stderr="")

    monkeypatch.setattr(run.subprocess, "run", fake_subprocess_run)

    result = run.run_single("runner_test_task", "runner_test_mode", alias, 0)

    env = captured["env"]
    assert isinstance(env, dict)
    assert env["PATH"] == f"/opt/tilth/bin{os.pathsep}/usr/bin"
    assert "SENTINEL_SECRET" not in env
    assert captured["stdin"] is run.subprocess.DEVNULL
    if runner == "codex":
        assert parser_calls == [("{}", model_id)]
    else:
        assert parser_calls == [("{}",)]

    cmd = captured["cmd"]
    assert isinstance(cmd, list)
    if runner == "claude":
        assert cmd[:2] == ["claude", "-p"]
        assert "--output-format" in cmd and "stream-json" in cmd
        assert "--strict-mcp-config" in cmd
        assert ["--mcp-config", "/controlled/tilth_mcp.json"] == cmd[-4:-2]
    elif runner == "codex":
        assert cmd[:3] == ["codex", "exec", "--json"]
        assert "--full-auto" in cmd and "--ephemeral" in cmd
        assert "-c" in cmd and any("mcp_servers.tilth.command" in arg for arg in cmd)
    else:
        assert cmd[:4] == ["opencode", "run", "--format", "json"]
        assert "--dir" in cmd and "--model" in cmd
        assert "--dangerously-skip-permissions" in cmd

    assert result["model"] == model_id
    assert result["model_alias"] == alias
    assert result["capability"] == "trace"
    assert result["source"] == {
        "origin": "fixture",
        "license": "MIT",
        "commit_or_tag": "test-pin",
        "transformation": "test-only",
    }
    assert result["per_turn_output_tokens"] == [17]
    assert result["cache_creation_5m_tokens"] == 1
    assert result["cache_creation_1h_tokens"] == 1
    assert result["per_turn_token_usage"] == [
        {
            "input_tokens": 3,
            "cache_creation_tokens": 2,
            "cache_creation_5m_tokens": 1,
            "cache_creation_1h_tokens": 1,
            "cache_read_tokens": 1,
            "output_tokens": 17,
        }
    ]



def test_claude_streaming_run_does_not_inherit_stdin(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    task = _RunnerTask()
    monkeypatch.setitem(run.TASKS, "runner_stream_task", task)
    monkeypatch.setitem(run.MODELS, "haiku", "configured-claude-model")
    monkeypatch.setitem(run.RUNNERS, "haiku", "claude")
    monkeypatch.setitem(
        run.MODES,
        "runner_stream_mode",
        ModeConfig(
            name="runner_stream_mode",
            tools=["Read"],
            mcp_config_path=None,
            description="streaming test",
        ),
    )
    monkeypatch.setattr(run, "get_repo_path", lambda _: tmp_path)
    monkeypatch.setattr(run, "parse_stream_json", lambda _: RunResult(
        session_id="session",
        turns=[Turn(index=0, input_tokens=1, output_tokens=2, cache_creation_tokens=0, cache_read_tokens=0)],
        num_turns=1,
        total_cost_usd=0.01,
        duration_ms=1,
        duration_api_ms=1,
        total_input_tokens=1,
        total_output_tokens=2,
        total_cache_creation_tokens=0,
        total_cache_read_tokens=0,
        result_text="correct answer",
    ))

    class FakeProcess:
        def __init__(self) -> None:
            self.stdout = io.StringIO('{"type":"result"}\n')
            self.stderr = io.StringIO("")
            self.returncode = 0

        def wait(self) -> None:
            return None

        def kill(self) -> None:
            self.returncode = -9

    process = FakeProcess()
    captured: dict[str, object] = {}

    def fake_popen(command, **kwargs):
        captured["command"] = command
        captured.update(kwargs)
        return process

    monkeypatch.setattr(run.subprocess, "Popen", fake_popen)

    result = run.run_single(
        "runner_stream_task",
        "runner_stream_mode",
        "haiku",
        0,
        stream_log_path=tmp_path / "stream.jsonl",
    )

    assert captured["stdin"] is run.subprocess.DEVNULL
    assert (tmp_path / "stream.jsonl").read_text() == '{"type":"result"}\n'
    assert result["task"] == "runner_stream_task"
    assert result["model"] == "configured-claude-model"
