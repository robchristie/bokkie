from __future__ import annotations

import json
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

from sqlalchemy import select
from sqlalchemy.orm import Session, selectinload

from ..config import Settings
from ..enums import (
    ArtifactKind,
    PhaseAttemptStatus,
    PhaseName,
    PublishStrategy,
    ReviewGate,
    ReviewStatus,
    RunStage,
    RunStatus,
    WorkerState,
)
from ..models import (
    Artifact,
    Event,
    Lease,
    OperatorNote,
    PhaseAttempt,
    Project,
    Review,
    Run,
    Worker,
    WorkerHeartbeat,
)
from ..prompts import phase_role_for
from ..schemas import (
    ArtifactSummary,
    EventRead,
    ExecutePhaseResult,
    OperatorDecision,
    OperatorNoteIn,
    PhaseAttemptCompletionIn,
    PhaseAttemptEventIn,
    PhaseAttemptSummary,
    PhaseLeaseResponse,
    PlanPhaseResult,
    ProjectCreate,
    ProjectRead,
    PromoteRunIn,
    ReviewPhaseResult,
    ReviewSummary,
    RunCreate,
    RunRead,
    SpecPhaseResult,
    VerifyPhaseResult,
    WorkerCapabilities,
    WorkerHeartbeatIn,
    WorkerRead,
)
from .artifacts import ArtifactStore
from .repo_config import TaskConfig, load_repo_config

CHANGE_PHASES = [
    PhaseName.PLAN.value,
    PhaseName.PLAN_REVIEW.value,
    PhaseName.SPEC.value,
    PhaseName.SPEC_REVIEW.value,
    PhaseName.EXECUTE.value,
    PhaseName.VERIFY.value,
    PhaseName.FINAL_REVIEW.value,
]


class OrchestrationError(RuntimeError):
    pass


class OrchestratorService:
    def __init__(self, db: Session, settings: Settings) -> None:
        self.db = db
        self.settings = settings
        self.repo_config = load_repo_config(settings)
        self.artifact_store = ArtifactStore(settings.artifacts_dir)
        self.settings.runs_root.mkdir(parents=True, exist_ok=True)

    def create_project(self, data: ProjectCreate) -> Project:
        project = Project(**data.model_dump())
        self.db.add(project)
        self.db.commit()
        self.db.refresh(project)
        return project

    def create_run(self, data: RunCreate) -> Run:
        project = self.db.get(Project, data.project_id)
        if project is None:
            raise OrchestrationError("Project not found")
        task_name = data.task_name or ("change" if data.type.value == "change" else data.type.value)
        run = Run(
            project_id=project.id,
            type=data.type.value,
            task_name=task_name,
            objective=data.objective,
            success_criteria=data.success_criteria,
            risk_level=data.risk_level.value,
            budget=data.budget.model_dump(exclude_none=True),
            resource_profile=data.resource_profile.model_dump(exclude_none=True),
            current_stage=RunStage.PLAN.value,
            status=RunStatus.QUEUED.value,
            base_ref=data.base_ref or project.default_branch,
            branch_name="placeholder",
            run_root=str((self.settings.runs_root / "pending").resolve()),
            preferred_pool=data.resource_profile.pool,
            requires_internet=data.resource_profile.internet,
            required_secrets=data.resource_profile.secrets,
            publish_strategy=data.publish_strategy.value,
            latest_summary="Run created and queued for planning.",
            next_action="Lease plan phase",
        )
        self.db.add(run)
        self.db.flush()
        run.branch_name = f"bokkie/run-{run.id[:8]}"
        run.run_root = str((self.settings.runs_root / run.id).resolve())
        self._queue_phase_attempt(run=run, phase_name=PhaseName.PLAN.value, payload={})
        self._write_request_artifact(run)
        self._sync_status_artifact(run)
        self._add_event(run.id, None, "run.created", run.latest_summary, {"run_id": run.id})
        self.db.commit()
        self.db.refresh(run)
        return run

    def list_runs(self) -> list[Run]:
        statement = (
            select(Run)
            .options(
                selectinload(Run.phase_attempts),
                selectinload(Run.reviews),
                selectinload(Run.artifacts),
            )
            .order_by(Run.created_at.desc())
        )
        return list(self.db.scalars(statement))

    def list_projects(self) -> list[Project]:
        statement = select(Project).order_by(Project.name, Project.slug)
        return list(self.db.scalars(statement))

    def list_workers(self) -> list[Worker]:
        return list(self.db.scalars(select(Worker).order_by(Worker.host, Worker.id)))

    def get_run(self, run_id: str) -> Run:
        statement = (
            select(Run)
            .where(Run.id == run_id)
            .options(
                selectinload(Run.phase_attempts),
                selectinload(Run.reviews),
                selectinload(Run.artifacts),
                selectinload(Run.events),
            )
        )
        run = self.db.scalars(statement).first()
        if run is None:
            raise OrchestrationError("Run not found")
        return run

    def get_project(self, project_id: str) -> Project:
        project = self.db.get(Project, project_id)
        if project is None:
            raise OrchestrationError("Project not found")
        return project

    def get_phase_attempt(self, phase_attempt_id: str) -> PhaseAttempt:
        statement = (
            select(PhaseAttempt)
            .where(PhaseAttempt.id == phase_attempt_id)
            .options(
                selectinload(PhaseAttempt.artifacts),
                selectinload(PhaseAttempt.events),
                selectinload(PhaseAttempt.reviews),
                selectinload(PhaseAttempt.run),
            )
        )
        phase_attempt = self.db.scalars(statement).first()
        if phase_attempt is None:
            raise OrchestrationError("Phase attempt not found")
        return phase_attempt

    def register_worker(self, data: WorkerCapabilities) -> Worker:
        worker = self.db.get(Worker, data.id)
        payload = data.model_dump()
        if worker is None:
            worker = Worker(
                id=data.id,
                host=data.host,
                pools=data.pools,
                labels=data.labels,
                secrets=data.secrets,
                cpu_cores=data.cpu_cores,
                ram_gb=data.ram_gb,
                gpu_model=data.gpu_model,
                gpu_vram_gb=data.gpu_vram_gb,
                metadata_json=payload["metadata"],
                state=WorkerState.IDLE.value,
                current_load=0,
                last_seen_at=self._now(),
            )
            self.db.add(worker)
        else:
            worker.host = data.host
            worker.pools = data.pools
            worker.labels = data.labels
            worker.secrets = data.secrets
            worker.cpu_cores = data.cpu_cores
            worker.ram_gb = data.ram_gb
            worker.gpu_model = data.gpu_model
            worker.gpu_vram_gb = data.gpu_vram_gb
            worker.metadata_json = payload["metadata"]
            worker.last_seen_at = self._now()
            if worker.state == WorkerState.OFFLINE.value:
                worker.state = WorkerState.IDLE.value
        self.db.commit()
        self.db.refresh(worker)
        return worker

    def heartbeat_worker(self, worker_id: str, data: WorkerHeartbeatIn) -> Worker:
        worker = self.db.get(Worker, worker_id)
        if worker is None:
            raise OrchestrationError("Worker not found")
        worker.last_seen_at = self._now()
        worker.current_load = data.observed_load
        heartbeat = WorkerHeartbeat(
            worker_id=worker_id,
            observed_load=data.observed_load,
            payload=data.payload,
        )
        self.db.add(heartbeat)
        self.db.commit()
        self.db.refresh(worker)
        return worker

    def claim_phase_attempt(
        self, worker_id: str, target_phase_attempt_id: str | None = None
    ) -> PhaseLeaseResponse:
        worker = self.db.get(Worker, worker_id)
        if worker is None:
            raise OrchestrationError("Worker not found")
        self._expire_leases()
        phase_attempts = self._claimable_phase_attempts(target_phase_attempt_id=target_phase_attempt_id)
        for phase_attempt in self.db.scalars(phase_attempts):
            run = phase_attempt.run
            if run.status == RunStatus.PAUSED.value:
                continue
            if not self._worker_matches(worker, run, phase_attempt):
                continue
            operator_notes = self._consume_pending_notes(run)
            now = self._now()
            phase_attempt.status = PhaseAttemptStatus.RUNNING.value
            phase_attempt.worker_id = worker.id
            phase_attempt.started_at = phase_attempt.started_at or now
            run.current_worker_id = worker.id
            run.current_stage = phase_attempt.phase_name
            run.current_session_id = phase_attempt.thread_id
            run.status = RunStatus.RUNNING.value
            run.started_at = run.started_at or now
            worker.state = WorkerState.BUSY.value
            worker.current_load = 1
            lease = Lease(
                phase_attempt_id=phase_attempt.id,
                worker_id=worker.id,
                expires_at=now + timedelta(seconds=self.settings.lease_ttl_seconds),
            )
            self.db.add(lease)
            self._add_event(
                run.id,
                phase_attempt.id,
                "lease.acquired",
                f"Phase {phase_attempt.phase_name} leased",
                {"worker_id": worker.id},
            )
            self._sync_status_artifact(run)
            self.db.commit()
            self.db.refresh(phase_attempt)
            run = self.get_run(run.id)
            project = self.get_project(run.project_id)
            return PhaseLeaseResponse(
                leased=True,
                phase_attempt=self._phase_attempt_summary(phase_attempt),
                run=self.serialize_run(run),
                project=ProjectRead.model_validate(project),
                prior_patch_downloads=[
                    self._artifact_download_url(artifact.id)
                    for artifact in self._prior_patch_artifacts(run.id)
                ],
                input_artifacts=self._input_artifacts_for_phase(run.id, phase_attempt.phase_name),
                operator_notes=operator_notes,
                evaluator_commands=self._evaluator_commands_for_run(run),
            )
        self.db.commit()
        return PhaseLeaseResponse(leased=False)

    def _claimable_phase_attempts(self, target_phase_attempt_id: str | None = None):
        filters = [
            PhaseAttempt.status == PhaseAttemptStatus.QUEUED.value,
            Run.status.in_([RunStatus.QUEUED.value, RunStatus.RUNNING.value]),
        ]
        if target_phase_attempt_id:
            filters.append(PhaseAttempt.id == target_phase_attempt_id)
        return (
            select(PhaseAttempt)
            .join(Run)
            .where(*filters)
            .order_by(PhaseAttempt.created_at, PhaseAttempt.phase_index, PhaseAttempt.attempt_no)
            .with_for_update(skip_locked=True)
        )

    def add_event(self, phase_attempt_id: str, data: PhaseAttemptEventIn) -> Event:
        phase_attempt = self.db.get(PhaseAttempt, phase_attempt_id)
        if phase_attempt is None:
            raise OrchestrationError("Phase attempt not found")
        event = self._add_event(
            phase_attempt.run_id,
            phase_attempt_id,
            data.event_type,
            data.summary,
            data.payload,
            data.worker_id,
        )
        if data.event_type == "thread/started":
            thread = data.payload.get("params", {}).get("thread", {})
            phase_attempt.thread_id = thread.get("id")
            phase_attempt.run.current_session_id = phase_attempt.thread_id
        elif data.event_type == "turn/started":
            turn = data.payload.get("params", {}).get("turn", {})
            phase_attempt.last_turn_id = turn.get("id")
        self._sync_status_artifact(phase_attempt.run)
        self.db.commit()
        self.db.refresh(event)
        return event

    def claim_phase_notes(self, phase_attempt_id: str) -> list[str]:
        phase_attempt = self.db.get(PhaseAttempt, phase_attempt_id)
        if phase_attempt is None:
            raise OrchestrationError("Phase attempt not found")
        notes = self._consume_pending_notes(phase_attempt.run)
        self.db.commit()
        return notes

    def complete_phase_attempt(
        self, phase_attempt_id: str, data: PhaseAttemptCompletionIn
    ) -> PhaseAttempt:
        if not isinstance(data, PhaseAttemptCompletionIn):
            data = PhaseAttemptCompletionIn.model_validate(data)
        phase_attempt = self.db.get(PhaseAttempt, phase_attempt_id)
        if phase_attempt is None:
            raise OrchestrationError("Phase attempt not found")
        run = phase_attempt.run
        worker = self.db.get(Worker, data.worker_id)
        now = self._now()
        self._release_open_leases(phase_attempt.id, "completed" if data.success else "failed")
        if worker is not None:
            worker.state = WorkerState.IDLE.value
            worker.current_load = 0
        phase_attempt.result = data.result
        phase_attempt.error_text = data.error_text
        phase_attempt.completed_at = now
        run.current_worker_id = None
        run.current_session_id = phase_attempt.thread_id
        if not data.success:
            phase_attempt.status = PhaseAttemptStatus.FAILED.value
            run.status = RunStatus.FAILED.value
            run.next_action = "Inspect failed phase attempt"
            self._add_event(
                run.id,
                phase_attempt.id,
                "phase.failed",
                data.error_text,
                {"result": data.result},
            )
            self._sync_status_artifact(run)
            self.db.commit()
            return phase_attempt

        phase_attempt.status = PhaseAttemptStatus.COMPLETED.value
        summary = data.summary or data.result.get("summary")
        if summary:
            run.latest_summary = summary
        self._add_event(
            run.id,
            phase_attempt.id,
            "phase.completed",
            summary,
            {"result": data.result, "phase": phase_attempt.phase_name},
        )
        self._apply_phase_result(run, phase_attempt, data.result)
        self._sync_status_artifact(run)
        self.db.commit()
        self.db.refresh(phase_attempt)
        return phase_attempt

    def approve_run(self, run_id: str, decision: OperatorDecision) -> Run:
        run = self.get_run(run_id)
        review = self._latest_pending_review(run)
        now = self._now()
        review.status = ReviewStatus.APPROVED.value
        review.decision_reason = decision.reason
        review.decided_by = decision.actor
        review.decided_at = now
        if review.gate == ReviewGate.PLAN.value:
            self._queue_phase_attempt(
                run=run,
                phase_name=PhaseName.SPEC.value,
                payload={"approved_plan_review_id": review.id},
            )
            run.current_stage = RunStage.SPEC.value
            run.status = RunStatus.QUEUED.value
            run.next_action = "Lease spec phase"
        elif review.gate == ReviewGate.SPEC.value:
            self._queue_phase_attempt(
                run=run,
                phase_name=PhaseName.EXECUTE.value,
                payload={"approved_spec_review_id": review.id},
            )
            run.current_stage = RunStage.EXECUTE.value
            run.status = RunStatus.QUEUED.value
            run.next_action = "Lease execute phase"
        elif review.gate == ReviewGate.FINAL.value:
            run.current_stage = RunStage.DONE.value
            run.status = RunStatus.DONE.value
            run.completed_at = now
            if run.publish_strategy == PublishStrategy.PUSH.value and run.project.push_remote:
                run.next_action = "Run completed; branch is ready to push manually"
            else:
                run.next_action = "Run completed"
        self._add_event(run.id, None, "review.approved", decision.reason, {"gate": review.gate})
        self._sync_status_artifact(run)
        self.db.commit()
        return self.get_run(run.id)

    def reject_run(self, run_id: str, decision: OperatorDecision) -> Run:
        run = self.get_run(run_id)
        review = self._latest_pending_review(run)
        now = self._now()
        review.status = ReviewStatus.REJECTED.value
        review.decision_reason = decision.reason
        review.decided_by = decision.actor
        review.decided_at = now
        payload = {
            "rejection_reason": decision.reason,
            "review_payload": review.payload,
        }
        if review.gate == ReviewGate.PLAN.value:
            self._queue_phase_attempt(run=run, phase_name=PhaseName.PLAN.value, payload=payload)
            run.current_stage = RunStage.PLAN.value
            run.status = RunStatus.QUEUED.value
            run.next_action = "Re-plan after operator rejection"
        elif review.gate == ReviewGate.SPEC.value:
            self._queue_phase_attempt(run=run, phase_name=PhaseName.SPEC.value, payload=payload)
            run.current_stage = RunStage.SPEC.value
            run.status = RunStatus.QUEUED.value
            run.next_action = "Re-spec after operator rejection"
        elif review.gate == ReviewGate.FINAL.value:
            self._queue_phase_attempt(run=run, phase_name=PhaseName.EXECUTE.value, payload=payload)
            run.current_stage = RunStage.EXECUTE.value
            run.status = RunStatus.QUEUED.value
            run.next_action = "Re-execute after final review rejection"
        self._add_event(run.id, None, "review.rejected", decision.reason, {"gate": review.gate})
        self._sync_status_artifact(run)
        self.db.commit()
        return self.get_run(run.id)

    def pause_run(self, run_id: str) -> Run:
        run = self.get_run(run_id)
        run.status = RunStatus.PAUSED.value
        run.next_action = "Paused by operator"
        self._add_event(run.id, None, "run.paused", run.next_action, {})
        self._sync_status_artifact(run)
        self.db.commit()
        return self.get_run(run.id)

    def resume_run(self, run_id: str) -> Run:
        run = self.get_run(run_id)
        pending_review = self._latest_pending_review(run, raise_missing=False)
        run.status = RunStatus.WAITING_REVIEW.value if pending_review else RunStatus.QUEUED.value
        run.next_action = "Resume scheduling"
        self._add_event(run.id, None, "run.resumed", run.next_action, {})
        self._sync_status_artifact(run)
        self.db.commit()
        return self.get_run(run.id)

    def steer_run(self, run_id: str, data: OperatorNoteIn) -> Run:
        run = self.get_run(run_id)
        note = OperatorNote(run_id=run.id, note=data.note, created_by=data.created_by)
        self.db.add(note)
        run.next_action = "Apply operator note at the next safe boundary or active steer"
        self._add_event(run.id, None, "run.steered", data.note, {"created_by": data.created_by})
        self._sync_status_artifact(run)
        self.db.commit()
        return self.get_run(run.id)

    def promote_run(self, run_id: str, data: PromoteRunIn) -> Run:
        run = self.get_run(run_id)
        run.preferred_pool = data.pool
        for phase_attempt in run.phase_attempts:
            if phase_attempt.status == PhaseAttemptStatus.QUEUED.value:
                phase_attempt.requested_pool = data.pool
        run.next_action = f"Promoted to pool {data.pool}"
        self._add_event(run.id, None, "run.promoted", run.next_action, {"pool": data.pool})
        self._sync_status_artifact(run)
        self.db.commit()
        return self.get_run(run.id)

    def serialize_run(self, run: Run) -> RunRead:
        artifacts = [
            self._artifact_summary(artifact)
            for artifact in sorted(run.artifacts, key=lambda artifact: artifact.created_at)
        ]
        reviews = [
            ReviewSummary.model_validate(review)
            for review in sorted(run.reviews, key=lambda review: review.created_at)
        ]
        events = [
            EventRead.model_validate(event)
            for event in sorted(run.events, key=lambda event: event.id)[-100:]
        ]
        phase_attempts = [
            self._phase_attempt_summary(phase_attempt)
            for phase_attempt in sorted(
                run.phase_attempts,
                key=lambda phase: (phase.phase_index, phase.attempt_no, phase.created_at),
            )
        ]
        return RunRead(
            id=run.id,
            project_id=run.project_id,
            type=run.type,
            task_name=run.task_name,
            objective=run.objective,
            success_criteria=run.success_criteria,
            risk_level=run.risk_level,
            budget=run.budget,
            resource_profile=run.resource_profile,
            current_stage=run.current_stage,
            current_session_id=run.current_session_id,
            status=run.status,
            base_ref=run.base_ref,
            branch_name=run.branch_name,
            run_root=run.run_root,
            latest_summary=run.latest_summary,
            current_worker_id=run.current_worker_id,
            latest_verifier_result=run.latest_verifier_result,
            next_action=run.next_action,
            blockers=run.blockers,
            risk_flags=run.risk_flags,
            preferred_pool=run.preferred_pool,
            requires_internet=run.requires_internet,
            required_secrets=run.required_secrets,
            publish_strategy=run.publish_strategy,
            created_at=run.created_at,
            updated_at=run.updated_at,
            started_at=run.started_at,
            completed_at=run.completed_at,
            phase_attempts=phase_attempts,
            reviews=reviews,
            artifacts=artifacts,
            events=events,
        )

    def serialize_worker(self, worker: Worker) -> WorkerRead:
        return WorkerRead.model_validate(worker)

    def pending_reviews(self) -> list[Review]:
        statement = (
            select(Review)
            .where(Review.status == ReviewStatus.PENDING.value)
            .order_by(Review.created_at)
        )
        return list(self.db.scalars(statement))

    def create_artifact(
        self,
        *,
        run_id: str,
        phase_attempt_id: str | None,
        kind: str,
        name: str,
        storage_path: str,
        content_type: str,
        sha256: str,
        size_bytes: int,
        metadata: dict[str, Any],
    ) -> Artifact:
        artifact = Artifact(
            run_id=run_id,
            phase_attempt_id=phase_attempt_id,
            kind=kind,
            name=name,
            storage_path=storage_path,
            content_type=content_type,
            sha256=sha256,
            size_bytes=size_bytes,
            metadata_json=metadata,
        )
        self.db.add(artifact)
        self.db.commit()
        self.db.refresh(artifact)
        return artifact

    def get_artifact(self, artifact_id: str) -> Artifact:
        artifact = self.db.get(Artifact, artifact_id)
        if artifact is None:
            raise OrchestrationError("Artifact not found")
        return artifact

    def _apply_phase_result(
        self, run: Run, phase_attempt: PhaseAttempt, result_payload: dict[str, Any]
    ) -> None:
        phase_name = phase_attempt.phase_name
        if phase_name == PhaseName.PLAN.value:
            result = PlanPhaseResult.model_validate(result_payload)
            run.blockers = result.blockers
            run.risk_flags = result.risk_flags
            run.current_stage = RunStage.PLAN_REVIEW.value
            run.status = RunStatus.QUEUED.value
            run.next_action = "Lease plan review phase"
            self._write_bundle_artifact(
                run,
                phase_attempt,
                ArtifactKind.PROPOSAL.value,
                "plan/proposal.md",
                result.proposal_md.encode(),
                "text/markdown",
            )
            self._write_bundle_artifact(
                run,
                phase_attempt,
                ArtifactKind.DESIGN.value,
                "plan/design.md",
                result.design_md.encode(),
                "text/markdown",
            )
            self._write_bundle_artifact(
                run,
                phase_attempt,
                ArtifactKind.TASKS.value,
                "plan/tasks.md",
                result.tasks_md.encode(),
                "text/markdown",
            )
            self._write_bundle_artifact(
                run,
                phase_attempt,
                ArtifactKind.PLAN_JSON.value,
                "plan/plan.json",
                json.dumps(result.model_dump(), indent=2).encode(),
                "application/json",
            )
            self._queue_phase_attempt(
                run=run,
                phase_name=PhaseName.PLAN_REVIEW.value,
                payload={"source_phase_attempt_id": phase_attempt.id},
            )
        elif phase_name == PhaseName.PLAN_REVIEW.value:
            result = ReviewPhaseResult.model_validate(result_payload)
            self._write_bundle_artifact(
                run,
                phase_attempt,
                ArtifactKind.REVIEW_JSON.value,
                "plan/review.json",
                json.dumps(result.model_dump(), indent=2).encode(),
                "application/json",
            )
            self.db.add(
                Review(
                    run_id=run.id,
                    phase_attempt_id=phase_attempt.id,
                    gate=ReviewGate.PLAN.value,
                    status=ReviewStatus.PENDING.value,
                    summary=result.summary,
                    payload=result.model_dump(),
                )
            )
            run.current_stage = RunStage.PLAN_REVIEW.value
            run.status = RunStatus.WAITING_REVIEW.value
            run.next_action = result.next_action
        elif phase_name == PhaseName.SPEC.value:
            result = SpecPhaseResult.model_validate(result_payload)
            self._write_bundle_artifact(
                run,
                phase_attempt,
                ArtifactKind.PROGRAM.value,
                "exec/PROGRAM.md",
                result.program_md.encode(),
                "text/markdown",
            )
            self._write_bundle_artifact(
                run,
                phase_attempt,
                ArtifactKind.SPEC_JSON.value,
                "exec/spec.json",
                json.dumps(result.model_dump(), indent=2).encode(),
                "application/json",
            )
            self._queue_phase_attempt(
                run=run,
                phase_name=PhaseName.SPEC_REVIEW.value,
                payload={"source_phase_attempt_id": phase_attempt.id},
            )
            run.current_stage = RunStage.SPEC_REVIEW.value
            run.status = RunStatus.QUEUED.value
            run.next_action = "Lease spec review phase"
        elif phase_name == PhaseName.SPEC_REVIEW.value:
            result = ReviewPhaseResult.model_validate(result_payload)
            self._write_bundle_artifact(
                run,
                phase_attempt,
                ArtifactKind.REVIEW_JSON.value,
                "exec/spec_review.json",
                json.dumps(result.model_dump(), indent=2).encode(),
                "application/json",
            )
            self.db.add(
                Review(
                    run_id=run.id,
                    phase_attempt_id=phase_attempt.id,
                    gate=ReviewGate.SPEC.value,
                    status=ReviewStatus.PENDING.value,
                    summary=result.summary,
                    payload=result.model_dump(),
                )
            )
            run.current_stage = RunStage.SPEC_REVIEW.value
            run.status = RunStatus.WAITING_REVIEW.value
            run.next_action = result.next_action
        elif phase_name == PhaseName.EXECUTE.value:
            result = ExecutePhaseResult.model_validate(result_payload)
            self._write_bundle_artifact(
                run,
                phase_attempt,
                ArtifactKind.CHECKPOINT.value,
                f"exec/checkpoints/{phase_attempt.id}.json",
                json.dumps(result.model_dump(), indent=2).encode(),
                "application/json",
            )
            run.current_stage = RunStage.VERIFY.value
            run.status = RunStatus.QUEUED.value
            run.next_action = result.next_action or "Lease verify phase"
            self._queue_phase_attempt(
                run=run,
                phase_name=PhaseName.VERIFY.value,
                payload={"source_phase_attempt_id": phase_attempt.id},
            )
        elif phase_name == PhaseName.VERIFY.value:
            result = VerifyPhaseResult.model_validate(result_payload)
            run.latest_verifier_result = result.model_dump(by_alias=True)
            self._write_bundle_artifact(
                run,
                phase_attempt,
                ArtifactKind.VERIFY_RESULTS.value,
                "verify/results.json",
                json.dumps(result.model_dump(by_alias=True), indent=2).encode(),
                "application/json",
            )
            self._queue_phase_attempt(
                run=run,
                phase_name=PhaseName.FINAL_REVIEW.value,
                payload={"source_phase_attempt_id": phase_attempt.id},
            )
            run.current_stage = RunStage.FINAL_REVIEW.value
            run.status = RunStatus.QUEUED.value
            run.next_action = "Lease final review phase"
        elif phase_name == PhaseName.FINAL_REVIEW.value:
            result = ReviewPhaseResult.model_validate(result_payload)
            review_text = [
                f"Verdict: {result.verdict}",
                "",
                result.summary,
            ]
            if result.concerns:
                review_text.extend(["", "Concerns:"])
                review_text.extend(f"- {concern}" for concern in result.concerns)
            self._write_bundle_artifact(
                run,
                phase_attempt,
                ArtifactKind.VERIFY_REVIEW.value,
                "verify/review.md",
                "\n".join(review_text).encode(),
                "text/markdown",
            )
            self.db.add(
                Review(
                    run_id=run.id,
                    phase_attempt_id=phase_attempt.id,
                    gate=ReviewGate.FINAL.value,
                    status=ReviewStatus.PENDING.value,
                    summary=result.summary,
                    payload=result.model_dump(),
                )
            )
            run.current_stage = RunStage.FINAL_REVIEW.value
            run.status = RunStatus.WAITING_REVIEW.value
            run.next_action = result.next_action
        else:
            raise OrchestrationError(f"Unsupported phase {phase_name}")

    def _queue_phase_attempt(
        self,
        *,
        run: Run,
        phase_name: str,
        payload: dict[str, Any],
    ) -> PhaseAttempt:
        attempts_for_phase = [phase for phase in run.phase_attempts if phase.phase_name == phase_name]
        phase_attempt = PhaseAttempt(
            run_id=run.id,
            phase_name=phase_name,
            phase_index=self._phase_index(run, phase_name),
            attempt_no=len(attempts_for_phase) + 1,
            role=phase_role_for(phase_name),
            requested_pool=run.preferred_pool,
            requires_internet=self._phase_requires_internet(run, phase_name),
            required_secrets=run.required_secrets,
            required_labels=self._required_labels(run),
            timeout_seconds=self._task_config_for_run(run).timeout_seconds,
            branch_name=run.branch_name,
            payload=payload,
        )
        self.db.add(phase_attempt)
        return phase_attempt

    def _task_config_for_run(self, run: Run) -> TaskConfig:
        default = TaskConfig(name="change")
        if run.task_name and run.task_name in self.repo_config.tasks:
            return self.repo_config.tasks[run.task_name]
        if run.type in self.repo_config.tasks:
            return self.repo_config.tasks[run.type]
        return default

    def _phase_index(self, run: Run, phase_name: str) -> int:
        run_type_config = self.repo_config.run_types.get(run.type)
        phases = run_type_config.phases if run_type_config else CHANGE_PHASES
        try:
            return phases.index(phase_name)
        except ValueError as exc:
            raise OrchestrationError(f"Phase {phase_name} is not configured for run type {run.type}") from exc

    def _phase_requires_internet(self, run: Run, phase_name: str) -> bool:
        if phase_name in {PhaseName.PLAN.value, PhaseName.PLAN_REVIEW.value, PhaseName.SPEC_REVIEW.value, PhaseName.FINAL_REVIEW.value}:
            return True
        return run.requires_internet and phase_name == PhaseName.SPEC.value

    def _required_labels(self, run: Run) -> list[str]:
        labels = self._task_config_for_run(run).executor_labels.copy()
        if run.requires_internet and "internet" not in labels:
            labels.append("internet")
        return labels

    def _evaluator_commands_for_run(self, run: Run) -> list[str]:
        task_commands = self._task_config_for_run(run).evaluator_commands
        if task_commands:
            return task_commands
        return [str(command) for command in run.project.command_profiles.get("verify", [])]

    def _input_artifacts_for_phase(self, run_id: str, phase_name: str) -> dict[str, str]:
        names = ["request.md", "status.json"]
        if phase_name in {
            PhaseName.PLAN_REVIEW.value,
            PhaseName.SPEC.value,
            PhaseName.SPEC_REVIEW.value,
            PhaseName.EXECUTE.value,
            PhaseName.VERIFY.value,
            PhaseName.FINAL_REVIEW.value,
        }:
            names.extend(
                [
                    "plan/proposal.md",
                    "plan/design.md",
                    "plan/tasks.md",
                    "plan/plan.json",
                    "plan/review.json",
                ]
            )
        if phase_name in {
            PhaseName.SPEC_REVIEW.value,
            PhaseName.EXECUTE.value,
            PhaseName.VERIFY.value,
            PhaseName.FINAL_REVIEW.value,
        }:
            names.extend(["exec/PROGRAM.md", "exec/spec.json", "exec/spec_review.json"])
        if phase_name in {PhaseName.VERIFY.value, PhaseName.FINAL_REVIEW.value}:
            names.append("verify/results.json")
        artifacts: dict[str, str] = {}
        for name in names:
            artifact = self._latest_artifact_by_name(run_id, name)
            if artifact is not None:
                artifacts[name] = self._artifact_download_url(artifact.id)
        return artifacts

    def _write_request_artifact(self, run: Run) -> None:
        text = (
            f"# Request\n\n"
            f"Objective: {run.objective}\n\n"
            f"Success criteria: {run.success_criteria}\n\n"
            f"Run type: {run.type}\n"
            f"Risk level: {run.risk_level}\n"
        )
        self._write_bundle_artifact(
            run,
            None,
            ArtifactKind.REQUEST.value,
            "request.md",
            text.encode(),
            "text/markdown",
        )

    def _sync_status_artifact(self, run: Run) -> None:
        payload = {
            "run_id": run.id,
            "status": run.status,
            "current_stage": run.current_stage,
            "current_worker_id": run.current_worker_id,
            "current_session_id": run.current_session_id,
            "latest_summary": run.latest_summary,
            "next_action": run.next_action,
            "blockers": run.blockers,
            "risk_flags": run.risk_flags,
            "latest_verifier_result": run.latest_verifier_result,
            "phase_attempts": [
                {
                    "id": phase.id,
                    "phase_name": phase.phase_name,
                    "attempt_no": phase.attempt_no,
                    "status": phase.status,
                    "thread_id": phase.thread_id,
                    "last_turn_id": phase.last_turn_id,
                }
                for phase in sorted(
                    run.phase_attempts,
                    key=lambda value: (value.phase_index, value.attempt_no, value.created_at),
                )
            ],
        }
        self._write_bundle_artifact(
            run,
            None,
            ArtifactKind.STATUS.value,
            "status.json",
            json.dumps(payload, indent=2).encode(),
            "application/json",
        )

    def _write_bundle_artifact(
        self,
        run: Run,
        phase_attempt: PhaseAttempt | None,
        kind: str,
        relative_path: str,
        content: bytes,
        content_type: str,
    ) -> Artifact:
        stored = self.artifact_store.put_relative_bytes(Path(run.id) / relative_path, content)
        artifact = self._latest_artifact_by_name(run.id, relative_path)
        if artifact is None:
            artifact = Artifact(
                run_id=run.id,
                phase_attempt_id=phase_attempt.id if phase_attempt else None,
                kind=kind,
                name=relative_path,
                storage_path=stored.storage_path,
                content_type=content_type,
                sha256=stored.sha256,
                size_bytes=stored.size_bytes,
                metadata_json={"relative_path": relative_path},
            )
            self.db.add(artifact)
        else:
            artifact.phase_attempt_id = phase_attempt.id if phase_attempt else artifact.phase_attempt_id
            artifact.kind = kind
            artifact.storage_path = stored.storage_path
            artifact.content_type = content_type
            artifact.sha256 = stored.sha256
            artifact.size_bytes = stored.size_bytes
            artifact.metadata_json = {"relative_path": relative_path}
        return artifact

    def _latest_artifact_by_name(self, run_id: str, name: str) -> Artifact | None:
        statement = (
            select(Artifact)
            .where(Artifact.run_id == run_id, Artifact.name == name)
            .order_by(Artifact.created_at.desc())
        )
        return self.db.scalars(statement).first()

    def _prior_patch_artifacts(self, run_id: str) -> list[Artifact]:
        statement = (
            select(Artifact)
            .where(Artifact.run_id == run_id, Artifact.kind == ArtifactKind.PATCH.value)
            .order_by(Artifact.created_at)
        )
        return list(self.db.scalars(statement))

    def _release_open_leases(self, phase_attempt_id: str, reason: str) -> None:
        statement = select(Lease).where(
            Lease.phase_attempt_id == phase_attempt_id,
            Lease.released_at.is_(None),
        )
        now = self._now()
        for lease in self.db.scalars(statement):
            lease.released_at = now
            lease.release_reason = reason

    def _expire_leases(self) -> None:
        statement = (
            select(Lease)
            .join(PhaseAttempt)
            .where(Lease.released_at.is_(None), Lease.expires_at < self._now())
        )
        now = self._now()
        for lease in self.db.scalars(statement):
            lease.released_at = now
            lease.release_reason = "expired"
            phase_attempt = lease.phase_attempt
            worker = self.db.get(Worker, lease.worker_id)
            if worker is not None:
                worker.state = WorkerState.IDLE.value
                worker.current_load = 0
            if phase_attempt.status == PhaseAttemptStatus.RUNNING.value:
                phase_attempt.status = PhaseAttemptStatus.QUEUED.value
                phase_attempt.worker_id = None
                phase_attempt.retry_count += 1
                if phase_attempt.retry_count > phase_attempt.retry_limit:
                    phase_attempt.status = PhaseAttemptStatus.FAILED.value
                    phase_attempt.run.status = RunStatus.FAILED.value
                    phase_attempt.run.next_action = "Phase attempt exceeded retry limit"
        self.db.flush()

    def _worker_matches(self, worker: Worker, run: Run, phase_attempt: PhaseAttempt) -> bool:
        if phase_attempt.requested_pool and phase_attempt.requested_pool not in worker.pools:
            return False
        if phase_attempt.requires_internet and "internet" not in worker.labels:
            return False
        if any(label not in worker.labels for label in phase_attempt.required_labels):
            return False
        if any(secret not in worker.secrets for secret in phase_attempt.required_secrets):
            return False
        return not worker.current_load > 0

    def _consume_pending_notes(self, run: Run) -> list[str]:
        notes = []
        now = self._now()
        for note in run.notes:
            if note.applied_at is None:
                note.applied_at = now
                notes.append(note.note)
        return notes

    def _latest_pending_review(self, run: Run, *, raise_missing: bool = True) -> Review | None:
        for review in sorted(run.reviews, key=lambda item: item.created_at, reverse=True):
            if review.status == ReviewStatus.PENDING.value:
                return review
        if raise_missing:
            raise OrchestrationError("Run has no pending review")
        return None

    def _add_event(
        self,
        run_id: str,
        phase_attempt_id: str | None,
        event_type: str,
        summary: str | None,
        payload: dict[str, Any],
        worker_id: str | None = None,
    ) -> Event:
        event = Event(
            run_id=run_id,
            phase_attempt_id=phase_attempt_id,
            worker_id=worker_id,
            event_type=event_type,
            summary=summary,
            payload=payload,
        )
        self.db.add(event)
        return event

    def _artifact_download_url(self, artifact_id: str) -> str:
        return f"{self.settings.api_base_url.rstrip('/')}/api/artifacts/{artifact_id}/download"

    def _artifact_summary(self, artifact: Artifact) -> ArtifactSummary:
        return ArtifactSummary(
            id=artifact.id,
            kind=artifact.kind,
            name=artifact.name,
            content_type=artifact.content_type,
            size_bytes=artifact.size_bytes,
            metadata_json=artifact.metadata_json,
            created_at=artifact.created_at,
            download_url=self._artifact_download_url(artifact.id),
        )

    def _phase_attempt_summary(self, phase_attempt: PhaseAttempt) -> PhaseAttemptSummary:
        return PhaseAttemptSummary.model_validate(phase_attempt)

    def _now(self) -> datetime:
        return datetime.now(tz=UTC)
