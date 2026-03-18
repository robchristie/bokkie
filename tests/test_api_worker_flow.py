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

    def run(self, worktree, prompt, schema_model, kind, on_event=None):
        self.calls += 1
        if on_event:
            on_event({"type": "thread.started", "thread_id": f"thr-{self.calls}"})
            on_event({"type": "turn.completed"})
        if kind.value == "plan":
            result = {
                "summary": "Plan ready",
                "next_action": "Wait for approval",
                "blockers": [],
                "risk_flags": [],
                "work_items": [{"title": "Implement feature", "instructions": "Edit the file"}],
            }
        elif kind.value == "implement":
            (worktree / "feature.txt").write_text("implemented\n")
            result = {
                "summary": "Feature implemented",
                "changed_files": ["feature.txt"],
                "next_action": "Run verifier",
            }
        else:
            result = {
                "summary": "Verification passed",
                "pass": True,
                "findings": [],
                "confidence": "high",
                "next_action": "Await approval",
            }
        return CodexRunResult(final_output=result, raw_last_message=json.dumps(result))


def test_worker_executes_run_via_api(tmp_path, settings: Settings, session) -> None:
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
            "command_profiles": {"verify": ["pytest -q"]},
        },
    )
    project_resp.raise_for_status()
    project_id = project_resp.json()["id"]

    run_resp = client.post(
        "/api/runs",
        json={
            "project_id": project_id,
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
            labels=[],
            secrets=[],
            metadata={},
        ),
        api_base_url="http://testserver",
    )
    worker.client = client
    worker.codex = FakeCodex()

    worker.register()
    lease = worker.lease()
    assert lease.leased is True
    worker.execute_assignment(lease)

    waiting_review = client.get(f"/api/runs/{run_id}").json()
    assert waiting_review["status"] == "waiting_review"

    client.post(f"/api/runs/{run_id}/approve", json={"actor": "tester"}).raise_for_status()
    lease = worker.lease()
    assert lease.work_item is not None
    assert lease.work_item.kind.value == "implement"
    worker.execute_assignment(lease)

    lease = worker.lease()
    assert lease.work_item is not None
    assert lease.work_item.kind.value == "verify"
    worker.execute_assignment(lease)

    client.post(f"/api/runs/{run_id}/approve", json={"actor": "tester"}).raise_for_status()
    final_run = client.get(f"/api/runs/{run_id}").json()
    assert final_run["status"] == "done"
    patch_artifacts = [
        artifact for artifact in final_run["artifacts"] if artifact["kind"] == "patch"
    ]
    assert patch_artifacts
