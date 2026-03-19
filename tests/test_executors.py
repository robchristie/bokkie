from __future__ import annotations

from bokkie.config import Settings
from bokkie.schemas import ProjectCreate, RunCreate
from bokkie.services.executors import ExecutorLauncherService
from bokkie.services.orchestrator import OrchestratorService


def test_dispatcher_launches_targeted_local_worker(tmp_path, session, monkeypatch) -> None:
    repo_root = tmp_path / "repo-root"
    (repo_root / "tasks").mkdir(parents=True)
    (repo_root / "bokkie.toml").write_text(
        """
[run_types.change]
phases = ["plan", "plan_review", "spec", "spec_review", "execute", "verify", "final_review"]

[executors.local]
driver = "local"
pools = ["cpu-large"]
labels = ["cpu", "internet"]
workdir = "."
"""
    )
    (repo_root / "tasks" / "change.toml").write_text(
        'run_type = "change"\nexecutor_labels = ["cpu"]\n'
    )
    settings = Settings(
        database_url="sqlite:///:memory:",
        repo_root=repo_root,
        bokkie_config_path=repo_root / "bokkie.toml",
        runs_root=tmp_path / ".bokkie" / "runs",
        artifacts_dir=tmp_path / ".bokkie" / "runs",
    )
    service = OrchestratorService(session, settings)
    project = service.create_project(
        ProjectCreate(
            slug="demo",
            name="Demo",
            repo_url="/tmp/demo.git",
        )
    )
    run = service.create_run(
        RunCreate(
            project_id=project.id,
            type="change",
            objective="Launch on executor",
            success_criteria="Dispatcher targets a single phase",
            resource_profile={"pool": "cpu-large", "internet": False, "secrets": []},
        )
    )
    launcher = ExecutorLauncherService(session, settings)

    captured: dict[str, object] = {}

    def fake_popen(command, **kwargs):
        captured["command"] = command
        captured["cwd"] = kwargs.get("cwd")

        class DummyProcess:
            pass

        return DummyProcess()

    monkeypatch.setattr("bokkie.services.executors.subprocess.Popen", fake_popen)
    launches = launcher.dispatch_once()

    assert launches == [run.phase_attempts[0].id]
    command = captured["command"]
    assert command[:2] == ["zsh", "-lc"]
    assert "--once" in command[2]
    assert "--target-phase-attempt-id" in command[2]
    assert run.phase_attempts[0].id in command[2]
    assert captured["cwd"] == "."
