from __future__ import annotations

from bokkie.enums import ReviewStatus, RunStage, RunStatus, WorkItemKind
from bokkie.schemas import (
    OperatorDecision,
    PlanResult,
    PlanWorkItemSpec,
    ProjectCreate,
    RunCreate,
    VerifyResult,
    WorkerCapabilities,
    WorkItemCompletionIn,
)
from bokkie.services.orchestrator import OrchestratorService


def create_project(service: OrchestratorService) -> str:
    project = service.create_project(
        ProjectCreate(
            slug="demo",
            name="Demo",
            repo_url="/tmp/demo.git",
            default_branch="main",
            command_profiles={"verify": ["pytest -q"]},
        )
    )
    return project.id


def test_run_lifecycle_from_plan_to_done(session, settings) -> None:
    service = OrchestratorService(session, settings)
    project_id = create_project(service)
    run = service.create_run(
        RunCreate(
            project_id=project_id,
            objective="Implement auth refactor",
            success_criteria="Auth responsibilities are split cleanly.",
        )
    )

    assert run.current_stage == RunStage.PLANNING.value
    assert run.status == RunStatus.QUEUED.value
    plan_item = run.work_items[0]
    assert plan_item.kind == WorkItemKind.PLAN.value

    service.complete_work_item(
        plan_item.id,
        data=WorkItemCompletionIn(
            success=True,
            worker_id="worker-a",
            summary="Planning complete",
            result=PlanResult(
                summary="Split auth into parser and session modules.",
                next_action="Await approval",
                work_items=[
                    PlanWorkItemSpec(
                        title="Refactor auth",
                        instructions="Split parsing and session loading.",
                    )
                ],
            ).model_dump(),
        ),
    )
    run = service.get_run(run.id)
    assert run.status == RunStatus.WAITING_REVIEW.value
    assert run.current_stage == RunStage.REVIEW_GATE_PLAN.value
    assert run.reviews[0].status == ReviewStatus.PENDING.value

    service.approve_run(run.id, OperatorDecision(actor="tester"))
    run = service.get_run(run.id)
    assert run.current_stage == RunStage.EXECUTE.value
    implement_item = next(
        item for item in run.work_items if item.kind == WorkItemKind.IMPLEMENT.value
    )

    service.complete_work_item(
        implement_item.id,
        data=WorkItemCompletionIn(
            success=True,
            worker_id="worker-a",
            summary="Implementation complete",
            result={
                "summary": "Changed auth modules",
                "changed_files": ["auth.py", "session.py"],
                "next_action": "Run verifier",
            },
        ),
    )
    run = service.get_run(run.id)
    verify_item = next(item for item in run.work_items if item.kind == WorkItemKind.VERIFY.value)
    assert verify_item.status == "queued"

    service.complete_work_item(
        verify_item.id,
        data=WorkItemCompletionIn(
            success=True,
            worker_id="worker-a",
            summary="Verification complete",
            result=VerifyResult(
                summary="Looks good.",
                pass_=True,
                findings=[],
                confidence="high",
                next_action="Await final approval",
            ).model_dump(by_alias=True),
        ),
    )
    run = service.get_run(run.id)
    assert run.status == RunStatus.WAITING_REVIEW.value
    assert run.current_stage == RunStage.REVIEW_GATE_VERIFY.value

    service.approve_run(run.id, OperatorDecision(actor="tester"))
    run = service.get_run(run.id)
    assert run.status == RunStatus.DONE.value
    assert run.current_stage == RunStage.PUBLISH.value


def test_claim_requires_matching_capabilities_and_requeues_expired_items(session, settings) -> None:
    service = OrchestratorService(session, settings)
    project_id = create_project(service)
    run = service.create_run(
        RunCreate(
            project_id=project_id,
            objective="Fix build",
            success_criteria="Build passes",
        )
    )
    item = run.work_items[0]
    item.requested_pool = "gpu-3090"
    item.required_secrets = ["forgejo"]
    item.requires_internet = True
    session.commit()

    service.register_worker(
        WorkerCapabilities(
            id="worker-a",
            host="app-server",
            pools=["cpu-large"],
            labels=[],
            secrets=[],
            metadata={},
        )
    )
    service.register_worker(
        WorkerCapabilities(
            id="worker-b",
            host="devbox",
            pools=["gpu-3090"],
            labels=["internet"],
            secrets=["forgejo"],
            metadata={},
        )
    )

    miss = service.claim_work_item("worker-a")
    assert miss.leased is False

    lease = service.claim_work_item("worker-b")
    assert lease.leased is True
    assert lease.work_item is not None
    leased_item = session.get(type(item), lease.work_item.id)
    leased_item.lease_expires_at = service._now().replace(year=2000)
    session.commit()

    service._expire_leases()
    session.commit()
    requeued = session.get(type(item), leased_item.id)
    assert requeued.status == "queued"
    assert session.get(type(item), leased_item.id).retry_count == 1
