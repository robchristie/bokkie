from __future__ import annotations

from bokkie.config import Settings
from bokkie.schemas import PlanPhaseResult
from bokkie.services.codex import AppServerSession, CodexAppServerBackend, CodexRunResult


def test_app_server_session_prepares_runtime_home(tmp_path) -> None:
    auth_path = tmp_path / "mounted-auth.json"
    auth_path.write_text('{"token":"abc"}')
    config_path = tmp_path / "mounted-config.toml"
    config_path.write_text("model = 'gpt-5'\n")
    runtime_home = tmp_path / "runtime-home"

    session = AppServerSession(
        Settings(
            worker_cache_dir=tmp_path / "cache",
            codex_auth_json_path=auth_path,
            codex_config_toml_path=config_path,
            codex_runtime_home_dir=runtime_home,
        )
    )
    prepared = session._prepare_runtime_home()

    assert prepared == runtime_home
    assert (runtime_home / ".codex" / "auth.json").read_text() == '{"token":"abc"}'
    assert (runtime_home / ".codex" / "config.toml").read_text() == "model = 'gpt-5'\n"


class FakeBackend(CodexAppServerBackend):
    def run(self, *args, **kwargs) -> CodexRunResult:  # type: ignore[override]
        return CodexRunResult(
            thread_id="thread-1",
            turn_id="turn-1",
            final_output={
                "summary": "done",
                "next_action": "wait",
                "proposal_md": "# Proposal",
                "design_md": "# Design",
                "tasks_md": "# Tasks",
                "blockers": [],
                "risk_flags": [],
            },
            raw_last_message='{"summary":"done"}',
        )


def test_backend_result_shape_is_compatible_with_phase_schemas(tmp_path) -> None:
    backend = FakeBackend(Settings(worker_cache_dir=tmp_path / "cache"))
    result = backend.run(tmp_path, "plan", PlanPhaseResult, writable=False, internet=True)

    validated = PlanPhaseResult.model_validate(result.final_output)
    assert result.thread_id == "thread-1"
    assert result.turn_id == "turn-1"
    assert validated.summary == "done"
