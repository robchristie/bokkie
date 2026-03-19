from __future__ import annotations

from bokkie.enums import PhaseAttemptStatus, PhaseName, ReviewStatus, RunStage, RunStatus
from bokkie.schemas import (
    ExecutePhaseResult,
    OperatorDecision,
    PlanPhaseResult,
    ProjectCreate,
    ReviewPhaseResult,
    RunCreate,
    VerifyPhaseResult,
    WorkerCapabilities,
    WorkerHeartbeatIn,
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


def test_change_run_lifecycle_from_plan_to_done(session, settings) -> None:
    service = OrchestratorService(session, settings)
    project_id = create_project(service)
    run = service.create_run(
        RunCreate(
            project_id=project_id,
            type="change",
            objective="Implement auth refactor",
            success_criteria="Auth responsibilities are split cleanly.",
        )
    )

    assert run.current_stage == RunStage.PLAN.value
    assert run.status == RunStatus.QUEUED.value
    plan_phase = run.phase_attempts[0]
    assert plan_phase.phase_name == PhaseName.PLAN.value

    service.complete_phase_attempt(
        plan_phase.id,
        data={
            "success": True,
            "worker_id": "worker-a",
            "summary": "Planning complete",
            "result": PlanPhaseResult(
                summary="Split auth into parser and session modules.",
                next_action="Run a plan review",
                proposal_md="# Proposal",
                design_md="# Design",
                tasks_md="# Tasks",
            ).model_dump(),
        },
    )
    run = service.get_run(run.id)
    assert run.status == RunStatus.QUEUED.value
    assert run.current_stage == RunStage.PLAN_REVIEW.value
    plan_review = next(
        phase for phase in run.phase_attempts if phase.phase_name == PhaseName.PLAN_REVIEW.value
    )

    service.complete_phase_attempt(
        plan_review.id,
        data={
            "success": True,
            "worker_id": "worker-a",
            "summary": "Plan review complete",
            "result": ReviewPhaseResult(
                verdict="approve",
                summary="Plan is bounded and clear.",
                concerns=[],
                next_action="Await operator approval",
            ).model_dump(),
        },
    )
    run = service.get_run(run.id)
    assert run.status == RunStatus.WAITING_REVIEW.value
    assert run.reviews[0].status == ReviewStatus.PENDING.value

    service.approve_run(run.id, OperatorDecision(actor="tester"))
    run = service.get_run(run.id)
    assert run.current_stage == RunStage.SPEC.value
    spec_phase = next(
        phase for phase in run.phase_attempts if phase.phase_name == PhaseName.SPEC.value
    )

    service.complete_phase_attempt(
        spec_phase.id,
        data={
            "success": True,
            "worker_id": "worker-a",
            "summary": "Spec ready",
            "result": {
                "summary": "Execution contract ready",
                "next_action": "Review the spec",
                "program_md": "# PROGRAM",
                "acceptance_checks": ["pytest -q"],
            },
        },
    )
    spec_review = next(
        phase for phase in service.get_run(run.id).phase_attempts
        if phase.phase_name == PhaseName.SPEC_REVIEW.value
    )
    service.complete_phase_attempt(
        spec_review.id,
        data={
            "success": True,
            "worker_id": "worker-a",
            "summary": "Spec reviewed",
            "result": ReviewPhaseResult(
                verdict="approve",
                summary="Spec is executable.",
                concerns=[],
                next_action="Await operator approval",
            ).model_dump(),
        },
    )
    service.approve_run(run.id, OperatorDecision(actor="tester"))
    run = service.get_run(run.id)
    execute_phase = next(
        phase for phase in run.phase_attempts if phase.phase_name == PhaseName.EXECUTE.value
    )

    service.complete_phase_attempt(
        execute_phase.id,
        data={
            "success": True,
            "worker_id": "worker-a",
            "summary": "Implementation complete",
            "result": ExecutePhaseResult(
                summary="Changed auth modules",
                changed_files=["auth.py", "session.py"],
                checkpoints=["Ran focused checks"],
                next_action="Run verifier",
            ).model_dump(),
        },
    )
    verify_phase = next(
        phase for phase in service.get_run(run.id).phase_attempts
        if phase.phase_name == PhaseName.VERIFY.value
    )
    assert verify_phase.status == PhaseAttemptStatus.QUEUED.value

    service.complete_phase_attempt(
        verify_phase.id,
        data={
            "success": True,
            "worker_id": "worker-a",
            "summary": "Verification complete",
            "result": VerifyPhaseResult(
                summary="Looks good.",
                pass_=True,
                findings=[],
                confidence="high",
                next_action="Run final review",
                command_results=[],
            ).model_dump(by_alias=True),
        },
    )
    final_review_phase = next(
        phase for phase in service.get_run(run.id).phase_attempts
        if phase.phase_name == PhaseName.FINAL_REVIEW.value
    )
    service.complete_phase_attempt(
        final_review_phase.id,
        data={
            "success": True,
            "worker_id": "worker-a",
            "summary": "Final review complete",
            "result": ReviewPhaseResult(
                verdict="approve",
                summary="Ready to ship.",
                concerns=[],
                next_action="Await final approval",
            ).model_dump(),
        },
    )
    run = service.get_run(run.id)
    assert run.status == RunStatus.WAITING_REVIEW.value
    assert run.current_stage == RunStage.FINAL_REVIEW.value

    service.approve_run(run.id, OperatorDecision(actor="tester"))
    run = service.get_run(run.id)
    assert run.status == RunStatus.DONE.value
    assert run.current_stage == RunStage.DONE.value
    artifact_names = {artifact.name for artifact in run.artifacts}
    assert "request.md" in artifact_names
    assert "plan/proposal.md" in artifact_names
    assert "exec/PROGRAM.md" in artifact_names
    assert "verify/results.json" in artifact_names


def test_claim_requires_matching_capabilities_and_requeues_expired_attempts(session, settings) -> None:
    service = OrchestratorService(session, settings)
    project_id = create_project(service)
    run = service.create_run(
        RunCreate(
            project_id=project_id,
            objective="Fix build",
            success_criteria="Build passes",
        )
    )
    phase = run.phase_attempts[0]
    phase.requested_pool = "gpu-3090"
    phase.required_labels = ["internet"]
    phase.required_secrets = ["forgejo"]
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
    service.heartbeat_worker("worker-b", WorkerHeartbeatIn(observed_load=0))

    miss = service.claim_phase_attempt("worker-a")
    assert miss.leased is False

    lease = service.claim_phase_attempt("worker-b")
    assert lease.leased is True
    assert lease.phase_attempt is not None
    leased_phase = session.get(type(phase), lease.phase_attempt.id)
    open_lease = leased_phase.leases[0]
    open_lease.expires_at = service._now().replace(year=2000)
    session.commit()

    service._expire_leases()
    session.commit()
    requeued = session.get(type(phase), leased_phase.id)
    assert requeued.status == PhaseAttemptStatus.QUEUED.value
    assert requeued.retry_count == 1
