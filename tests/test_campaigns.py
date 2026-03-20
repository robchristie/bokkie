from __future__ import annotations

from pathlib import Path

from fastapi.testclient import TestClient

import bokkie.app as app_module
from bokkie.db import get_db
from bokkie.schemas import (
    AnalyzePhaseResult,
    CampaignDraftCreate,
    ExecutePhaseResult,
    OperatorDecision,
    OperatorNoteIn,
    PlanPhaseResult,
    ProjectCreate,
    ProposedIteration,
    ProposeNextPhaseResult,
    RunCreate,
    VerifyPhaseResult,
)
from bokkie.services.orchestrator import OrchestratorService


def create_project(service: OrchestratorService) -> str:
    project = service.create_project(
        ProjectCreate(
            slug="demo",
            name="Demo",
            repo_url="/tmp/demo.git",
            default_branch="main",
        )
    )
    return project.id


def create_client(session, settings) -> TestClient:
    app_module.settings = settings
    app = app_module.create_app()

    def override_get_db():
        yield session

    app.dependency_overrides[get_db] = override_get_db
    return TestClient(app)


def test_campaign_draft_api_approval_creates_campaign_and_first_run(session, settings) -> None:
    service = OrchestratorService(session, settings)
    create_project(service)
    client = create_client(session, settings)

    draft_response = client.post(
        "/api/campaign-drafts",
        json={
            "prompt": (
                "In demo, continue free-data event research. Keep it autonomous, "
                "prefer devbox, and cap spend at $15."
            )
        },
    )
    draft_response.raise_for_status()
    draft = draft_response.json()
    assert draft["draft"]["campaign_type"] == "research"
    assert draft["draft"]["first_run_type"] == "experiment"
    assert draft["draft"]["budget"]["max_total_cost"] == 15.0

    campaign_response = client.post(f"/api/campaign-drafts/{draft['id']}/approve", json={})
    campaign_response.raise_for_status()
    campaign = campaign_response.json()

    assert campaign["current_iteration_no"] == 1
    assert campaign["runs"][0]["campaign_id"] == campaign["id"]
    assert campaign["runs"][0]["iteration_no"] == 1
    assert Path(campaign["notebook_path"]).exists()
    assert (Path(campaign["artifact_root"]) / "setup" / "draft.json").exists()


def test_campaign_steering_notes_are_stored_and_claimed_by_active_run(session, settings) -> None:
    service = OrchestratorService(session, settings)
    project_id = create_project(service)
    run = service.create_run(
        RunCreate(
            project_id=project_id,
            objective="Initial run",
            success_criteria="Collect one iteration",
        )
    )
    draft = service.create_campaign_draft(
        CampaignDraftCreate(
            prompt="In demo, continue event research. Keep it autonomous.",
            project_id=project_id,
        )
    )
    campaign = service.approve_campaign_draft(draft.id, OperatorDecision(actor="tester"))
    active_run = sorted(campaign.runs, key=lambda item: item.iteration_no or 0)[-1]
    phase_id = active_run.phase_attempts[0].id

    service.steer_campaign(
        campaign.id,
        OperatorNoteIn(note="Pause before changing data family", created_by="tester"),
    )
    claimed_notes = service.claim_phase_notes(phase_id)

    assert claimed_notes == ["Pause before changing data family"]
    stored_note = service.get_campaign(campaign.id).notes[0]
    assert stored_note.applied_at is not None
    assert stored_note.campaign_id == campaign.id
    assert run.id != active_run.id


def test_campaign_pages_render(session, settings) -> None:
    service = OrchestratorService(session, settings)
    project_id = create_project(service)
    draft = service.create_campaign_draft(
        CampaignDraftCreate(
            prompt="In demo, continue event research. Keep it autonomous.",
            project_id=project_id,
        )
    )
    campaign = service.approve_campaign_draft(draft.id, OperatorDecision(actor="tester"))
    client = create_client(session, settings)

    intake = client.get(f"/ui/intake?draft_id={draft.id}")
    assert intake.status_code == 200
    assert "Draft Proposal" in intake.text

    campaigns = client.get("/ui/campaigns")
    assert campaigns.status_code == 200
    assert campaign.title in campaigns.text

    detail = client.get(f"/ui/campaigns/{campaign.id}")
    assert detail.status_code == 200
    assert campaign.objective in detail.text
    assert "Steering History" in detail.text


def test_experiment_campaign_auto_continue_creates_next_iteration(session, settings) -> None:
    service = OrchestratorService(session, settings)
    project_id = create_project(service)
    draft = service.create_campaign_draft(
        CampaignDraftCreate(
            prompt=(
                "In demo, continue event research. Keep it autonomous, "
                "cap spend at $15, and stay on the same data family."
            ),
            project_id=project_id,
        )
    )
    campaign = service.approve_campaign_draft(draft.id, OperatorDecision(actor="tester"))
    run = sorted(campaign.runs, key=lambda item: item.iteration_no or 0)[-1]
    plan_phase = run.phase_attempts[0]

    service.complete_phase_attempt(
        plan_phase.id,
        {
            "success": True,
            "worker_id": "worker-a",
            "summary": "Plan ready",
            "result": PlanPhaseResult(
                summary="Bound the next experiment.",
                next_action="Execute the bounded experiment",
                proposal_md="# Proposal",
                design_md="# Design",
                tasks_md="# Tasks",
            ).model_dump(),
        },
    )
    execute_phase = next(
        phase for phase in service.get_run(run.id).phase_attempts if phase.phase_name == "execute"
    )
    service.complete_phase_attempt(
        execute_phase.id,
        {
            "success": True,
            "worker_id": "worker-a",
            "summary": "Execution complete",
            "result": ExecutePhaseResult(
                summary="Executed one research pass",
                changed_files=["pipeline.py"],
                checkpoints=["wrote report inputs"],
                next_action="Verify outputs",
            ).model_dump(),
        },
    )
    verify_phase = next(
        phase for phase in service.get_run(run.id).phase_attempts if phase.phase_name == "verify"
    )
    service.complete_phase_attempt(
        verify_phase.id,
        {
            "success": True,
            "worker_id": "worker-a",
            "summary": "Verification done",
            "result": VerifyPhaseResult(
                summary="Verification passed",
                pass_=True,
                findings=[],
                confidence="high",
                next_action="Analyze the results",
                command_results=[],
            ).model_dump(by_alias=True),
        },
    )
    analyze_phase = next(
        phase for phase in service.get_run(run.id).phase_attempts if phase.phase_name == "analyze"
    )
    service.complete_phase_attempt(
        analyze_phase.id,
        {
            "success": True,
            "worker_id": "worker-a",
            "summary": "Analysis done",
            "result": AnalyzePhaseResult(
                summary="The event family still looks promising.",
                key_findings=["Signal remains in the target family."],
                report_md="# Analysis",
                recommended_direction="Continue in the same branch.",
                data_family="free-data",
                research_branch="8-k",
            ).model_dump(),
        },
    )
    propose_next_phase = next(
        phase
        for phase in service.get_run(run.id).phase_attempts
        if phase.phase_name == "propose_next"
    )
    service.complete_phase_attempt(
        propose_next_phase.id,
        {
            "success": True,
            "worker_id": "worker-a",
            "summary": "Next step proposed",
            "result": ProposeNextPhaseResult(
                summary="Queue the next bounded 8-K iteration.",
                should_continue=True,
                within_policy=True,
                requires_operator_approval=False,
                approval_reason=None,
                recommended_action="Continue automatically",
                rationale=["Same data family and same executor class."],
                estimated_additional_cost=2.5,
                next_iteration=ProposedIteration(
                    objective="Run the next bounded 8-K study",
                    success_criteria="Produce another analysis checkpoint",
                    run_type="experiment",
                    task_name="research_iteration",
                    preferred_pool="cpu-large",
                    requires_internet=True,
                    data_family="free-data",
                    research_branch="8-k",
                    deliverables=["analysis/report.md"],
                ),
            ).model_dump(mode="json"),
        },
    )

    refreshed_campaign = service.get_campaign(campaign.id)
    refreshed_runs = sorted(refreshed_campaign.runs, key=lambda item: item.iteration_no or 0)
    assert len(refreshed_runs) == 2
    assert refreshed_runs[0].status == "done"
    assert refreshed_runs[1].iteration_no == 2
    assert refreshed_runs[1].status == "queued"
    assert refreshed_campaign.current_iteration_no == 2
