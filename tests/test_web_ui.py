from __future__ import annotations

from fastapi.testclient import TestClient

import bokkie.app as app_module
from bokkie.db import get_db
from bokkie.models import Project


def test_web_ui_forms_create_project_and_run(session, settings) -> None:
    app_module.settings = settings
    app = app_module.create_app()

    def override_get_db():
        yield session

    app.dependency_overrides[get_db] = override_get_db
    client = TestClient(app)

    project_response = client.post(
        "/ui/projects",
        data={
            "slug": "web-demo",
            "name": "Web Demo",
            "repo_url": "/tmp/web-demo.git",
            "default_branch": "main",
            "push_remote": "",
        },
        follow_redirects=False,
    )
    assert project_response.status_code == 303

    runs_page = client.get("/ui/runs")
    assert "Web Demo" in runs_page.text

    runs_response = client.get("/api/runs")
    assert runs_response.json() == []

    projects = session.query(Project).all()
    assert len(projects) == 1

    run_response = client.post(
        "/ui/runs",
        data={
            "project_id": projects[0].id,
            "objective": "Operate from the browser",
            "success_criteria": "Web UI can create and steer runs",
            "run_type": "feature",
            "risk_level": "medium",
            "pool": "cpu-large",
            "internet": "false",
        },
        follow_redirects=False,
    )
    assert run_response.status_code == 303
    location = run_response.headers["location"]
    detail = client.get(location)
    assert "Operate from the browser" in detail.text
