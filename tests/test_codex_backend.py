from __future__ import annotations

import json
from pathlib import Path

from bokkie.config import Settings
from bokkie.enums import WorkItemKind
from bokkie.schemas import PlanResult
from bokkie.services.codex import CodexCliBackend


class FakeProcess:
    def __init__(self, output_path: Path) -> None:
        self.stdin = FakeStdin()
        self.stdout = iter(['{"type":"turn.completed"}\n'])
        self.stderr = FakeStderr()
        self.output_path = output_path

    def wait(self) -> int:
        self.output_path.write_text(
            json.dumps(
                {
                    "summary": "done",
                    "next_action": "wait",
                    "blockers": [],
                    "risk_flags": [],
                    "work_items": [],
                }
            )
        )
        return 0


class FakeStdin:
    def write(self, _: str) -> None:
        return None

    def close(self) -> None:
        return None


class FakeStderr:
    def read(self) -> str:
        return ""


def test_codex_backend_seeds_runtime_home(monkeypatch, tmp_path) -> None:
    auth_path = tmp_path / "mounted-auth.json"
    auth_path.write_text('{"token":"abc"}')
    config_path = tmp_path / "mounted-config.toml"
    config_path.write_text("model = 'gpt-5'\n")
    runtime_home = tmp_path / "runtime-home"

    captured: dict[str, object] = {}

    def fake_popen(command, env=None, **kwargs):
        output_path = Path(command[command.index("--output-last-message") + 1])
        captured["env"] = env
        captured["command"] = command
        return FakeProcess(output_path)

    monkeypatch.setattr("bokkie.services.codex.subprocess.Popen", fake_popen)

    backend = CodexCliBackend(
        Settings(
            worker_cache_dir=tmp_path / "cache",
            codex_auth_json_path=auth_path,
            codex_config_toml_path=config_path,
            codex_runtime_home_dir=runtime_home,
        )
    )
    result = backend.run(
        worktree=tmp_path,
        prompt="plan",
        schema_model=PlanResult,
        kind=WorkItemKind.PLAN,
    )

    assert result.final_output["summary"] == "done"
    assert captured["env"]["HOME"] == str(runtime_home)
    assert (runtime_home / ".codex" / "auth.json").read_text() == '{"token":"abc"}'
    assert (runtime_home / ".codex" / "config.toml").read_text() == "model = 'gpt-5'\n"
