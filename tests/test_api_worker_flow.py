from __future__ import annotations

import json
import subprocess
from pathlib import Path

from fastapi.testclient import TestClient

import bokkie.app as app_module
from bokkie.config import Settings
from bokkie.db import get_db
from bokkie.schemas import WorkerCapabilities
from bokkie.services.codex import CodexRunResult
from bokkie.worker import WorkerRunner


def init_repo(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "-b", "main"], cwd=path, check=True, capture_output=True)
    (path / "README.md").write_text("# demo\n")
    subprocess.run(["git", "add", "."], cwd=path, check=True, capture_output=True)
    subprocess.run(
        [
            "git",
            "-c",
            "user.name=Tester",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "init",
        ],
        cwd=path,
        check=True,
        capture_output=True,
    )


class FakeCodex:
    def __init__(self) -> None:
        self.calls = 0

    def run(self, worktree, prompt, schema_model, **kwargs):
        self.calls += 1
        on_event = kwargs.get("on_event")
        if on_event:
            on_event(
                {
                    "method": "thread/started",
                    "params": {"thread": {"id": f"thr-{self.calls}"}},
                }
            )
            on_event(
                {
                    "method": "turn/started",
                    "params": {"turn": {"id": f"turn-{self.calls}"}},
                }
            )
            on_event({"method": "turn/completed", "params": {"turn": {"id": f"turn-{self.calls}"}}})
        if schema_model.__name__ == "PlanPhaseResult":
            result = {
                "summary": "Plan ready",
                "next_action": "Wait for approval",
                "proposal_md": "# Proposal",
                "design_md": "# Design",
                "tasks_md": "# Tasks",
                "blockers": [],
                "risk_flags": [],
            }
        elif schema_model.__name__ == "ReviewPhaseResult":
            result = {
                "verdict": "approve",
                "summary": "Looks good",
                "concerns": [],
                "next_action": "Await approval",
            }
        elif schema_model.__name__ == "SpecPhaseResult":
            result = {
                "summary": "Spec ready",
                "next_action": "Review the spec",
                "program_md": "# PROGRAM",
                "acceptance_checks": ["pytest -q"],
            }
        elif schema_model.__name__ == "ExecutePhaseResult":
            (worktree / "feature.txt").write_text("implemented\n")
            result = {
                "summary": "Feature implemented",
                "changed_files": ["feature.txt"],
                "checkpoints": ["wrote feature.txt"],
                "next_action": "Run verifier",
            }
        else:
            result = {
                "summary": "Verification passed",
                "pass": True,
                "findings": [],
                "confidence": "high",
                "next_action": "Run final review",
                "command_results": [],
            }
        return CodexRunResult(
            thread_id=f"thr-{self.calls}",
            turn_id=f"turn-{self.calls}",
            final_output=result,
            raw_last_message=json.dumps(result),
        )


def test_worker_executes_change_run_via_api(tmp_path, session) -> None:
    repo_root = tmp_path / "repo-root"
    (repo_root / "tasks").mkdir(parents=True)
    (repo_root / "bokkie.toml").write_text(
        """
[run_types.change]
phases = ["plan", "plan_review", "spec", "spec_review", "execute", "verify", "final_review"]
"""
    )
    (repo_root / "tasks" / "change.toml").write_text('run_type = "change"\n')
    settings = Settings(
        database_url="sqlite:///:memory:",
        api_base_url="http://testserver",
        repo_root=repo_root,
        bokkie_config_path=repo_root / "bokkie.toml",
        runs_root=tmp_path / ".bokkie" / "runs",
        artifacts_dir=tmp_path / ".bokkie" / "runs",
        worker_cache_dir=tmp_path / "cache",
        worker_worktree_dir=tmp_path / "worktrees",
        worker_cleanup_worktrees=True,
    )
    app_module.settings = settings
    app = app_module.create_app()

    def override_get_db():
        yield session

    app.dependency_overrides[get_db] = override_get_db
    client = TestClient(app)
    repo = tmp_path / "repo"
    init_repo(repo)

    project_resp = client.post(
        "/api/projects",
        json={
            "slug": "demo",
            "name": "Demo",
            "repo_url": str(repo),
            "default_branch": "main",
            "command_profiles": {"verify": []},
        },
    )
    project_resp.raise_for_status()
    project_id = project_resp.json()["id"]

    run_resp = client.post(
        "/api/runs",
        json={
            "project_id": project_id,
            "type": "change",
            "objective": "Build the feature",
            "success_criteria": "Feature lands cleanly",
            "resource_profile": {"pool": "cpu-large", "internet": False, "secrets": []},
        },
    )
    run_resp.raise_for_status()
    run_id = run_resp.json()["id"]

    worker = WorkerRunner(
        settings=settings,
        worker=WorkerCapabilities(
            id="worker-1",
            host="devbox",
            pools=["cpu-large"],
            labels=["internet", "cpu"],
            secrets=[],
            metadata={},
        ),
        api_base_url="http://testserver",
    )
    worker.client = client
    worker.codex = FakeCodex()

    worker.register()

    for _ in range(2):
        lease = worker.lease()
        assert lease.leased is True
        worker.execute_assignment(lease)
    waiting_review = client.get(f"/api/runs/{run_id}").json()
    assert waiting_review["status"] == "waiting_review"
    client.post(f"/api/runs/{run_id}/approve", json={"actor": "tester"}).raise_for_status()

    for _ in range(2):
        lease = worker.lease()
        assert lease.phase_attempt is not None
        worker.execute_assignment(lease)
    client.post(f"/api/runs/{run_id}/approve", json={"actor": "tester"}).raise_for_status()

    for _ in range(3):
        lease = worker.lease()
        assert lease.phase_attempt is not None
        worker.execute_assignment(lease)

    run_before_final = client.get(f"/api/runs/{run_id}").json()
    assert run_before_final["status"] == "waiting_review"
    client.post(f"/api/runs/{run_id}/approve", json={"actor": "tester"}).raise_for_status()
    final_run = client.get(f"/api/runs/{run_id}").json()
    assert final_run["status"] == "done"
    patch_artifacts = [
        artifact for artifact in final_run["artifacts"] if artifact["kind"] == "patch"
    ]
    artifact_names = {artifact["name"] for artifact in final_run["artifacts"]}
    assert patch_artifacts
    assert "exec/PROGRAM.md" in artifact_names
