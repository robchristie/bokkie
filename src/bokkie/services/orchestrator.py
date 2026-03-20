from __future__ import annotations

import json
import re
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

from sqlalchemy import select
from sqlalchemy.orm import Session, selectinload

from ..config import Settings
from ..enums import (
    ArtifactKind,
    CampaignDraftStatus,
    CampaignStatus,
    PhaseAttemptStatus,
    PhaseName,
    PublishStrategy,
    ReviewGate,
    ReviewStatus,
    RunStage,
    RunStatus,
    RunType,
    WorkerState,
)
from ..models import (
    Artifact,
    Campaign,
    CampaignDraft,
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
    AnalyzePhaseResult,
    ArtifactSummary,
    CampaignDraftApprove,
    CampaignDraftCreate,
    CampaignDraftPayload,
    CampaignDraftRead,
    CampaignFileLink,
    CampaignRead,
    ContinuationPolicy,
    EventRead,
    ExecutePhaseResult,
    OperatorDecision,
    OperatorNoteIn,
    OperatorNoteRead,
    PhaseAttemptCompletionIn,
    PhaseAttemptEventIn,
    PhaseAttemptSummary,
    PhaseLeaseResponse,
    PlanPhaseResult,
    ProjectCreate,
    ProjectRead,
    PromoteRunIn,
    ProposedIteration,
    ProposeNextPhaseResult,
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

EXPERIMENT_PHASES = [
    PhaseName.PLAN.value,
    PhaseName.EXECUTE.value,
    PhaseName.VERIFY.value,
    PhaseName.ANALYZE.value,
    PhaseName.PROPOSE_NEXT.value,
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
        self.settings.campaigns_root.mkdir(parents=True, exist_ok=True)

    def create_project(self, data: ProjectCreate) -> Project:
        project = Project(**data.model_dump())
        self.db.add(project)
        self.db.commit()
        self.db.refresh(project)
        return project

    def create_campaign_draft(self, data: CampaignDraftCreate) -> CampaignDraft:
        project = self._infer_project(data.prompt, explicit_project_id=data.project_id)
        payload = self._build_campaign_draft_payload(data.prompt, project)
        draft = CampaignDraft(
            project_id=project.id if project else None,
            operator_prompt=data.prompt,
            status=CampaignDraftStatus.DRAFT.value,
            draft_json=payload.model_dump(mode="json"),
        )
        self.db.add(draft)
        self.db.commit()
        self.db.refresh(draft)
        return draft

    def approve_campaign_draft(
        self, draft_id: str, decision: OperatorDecision, data: CampaignDraftApprove | None = None
    ) -> Campaign:
        draft = self.get_campaign_draft(draft_id)
        if draft.status != CampaignDraftStatus.DRAFT.value:
            raise OrchestrationError("Campaign draft is not pending")
        payload = self._apply_draft_overrides(
            CampaignDraftPayload.model_validate(draft.draft_json),
            data or CampaignDraftApprove(),
        )
        project_id = (
            (data.project_id if data else None) or payload.inferred_project_id or draft.project_id
        )
        if not project_id:
            raise OrchestrationError("Campaign draft does not resolve to a project")
        project = self.get_project(project_id)
        campaign = Campaign(
            project_id=project.id,
            title=payload.title,
            objective=payload.objective,
            status=CampaignStatus.ACTIVE.value,
            campaign_type=payload.campaign_type,
            task_name=payload.task_name,
            preferred_pool=payload.preferred_pool,
            requires_internet=payload.requires_internet,
            budget_json=payload.budget.model_dump(exclude_none=True),
            continuation_policy_json=payload.continuation_policy.model_dump(exclude_none=True),
            approval_gates_json=payload.approval_gates,
            latest_summary="Campaign approved. First iteration queued.",
            next_action="Lease first iteration",
            current_iteration_no=0,
            notebook_path=str((self.settings.campaigns_root / "pending" / "NOTEBOOK.md").resolve()),
            artifact_root=str((self.settings.campaigns_root / "pending").resolve()),
        )
        self.db.add(campaign)
        self.db.flush()
        campaign_root = self.settings.campaigns_root / campaign.id
        campaign.artifact_root = str(campaign_root.resolve())
        campaign.notebook_path = str((campaign_root / "NOTEBOOK.md").resolve())
        draft.project_id = project.id
        draft.campaign_id = campaign.id
        draft.status = CampaignDraftStatus.APPROVED.value
        draft.decision_reason = decision.reason
        draft.decided_by = decision.actor
        draft.decided_at = self._now()
        self.db.commit()
        self.db.refresh(campaign)
        self._write_campaign_setup_files(campaign, draft, payload)
        run = self.create_run(
            RunCreate(
                project_id=project.id,
                campaign_id=campaign.id,
                iteration_no=1,
                type=payload.first_run_type,
                task_name=payload.task_name,
                objective=payload.first_run_objective,
                success_criteria=payload.first_run_success_criteria,
                resource_profile={
                    "pool": payload.preferred_pool,
                    "internet": payload.requires_internet,
                    "secrets": [],
                },
            )
        )
        campaign = self.get_campaign(campaign.id)
        campaign.current_iteration_no = 1
        campaign.latest_summary = run.latest_summary
        campaign.next_action = run.next_action
        campaign.status = CampaignStatus.ACTIVE.value
        self._sync_campaign_from_run(run)
        self.db.commit()
        return self.get_campaign(campaign.id)

    def reject_campaign_draft(self, draft_id: str, decision: OperatorDecision) -> CampaignDraft:
        draft = self.get_campaign_draft(draft_id)
        draft.status = CampaignDraftStatus.REJECTED.value
        draft.decision_reason = decision.reason
        draft.decided_by = decision.actor
        draft.decided_at = self._now()
        self.db.commit()
        self.db.refresh(draft)
        return draft

    def create_run(self, data: RunCreate) -> Run:
        project = self.db.get(Project, data.project_id)
        if project is None:
            raise OrchestrationError("Project not found")
        task_name = data.task_name or ("change" if data.type.value == "change" else data.type.value)
        initial_phase = self._initial_phase_name(data.type.value)
        run = Run(
            project_id=project.id,
            campaign_id=data.campaign_id,
            iteration_no=data.iteration_no,
            type=data.type.value,
            task_name=task_name,
            objective=data.objective,
            success_criteria=data.success_criteria,
            risk_level=data.risk_level.value,
            budget=data.budget.model_dump(exclude_none=True),
            resource_profile=data.resource_profile.model_dump(exclude_none=True),
            current_stage=initial_phase,
            status=RunStatus.QUEUED.value,
            base_ref=data.base_ref or project.default_branch,
            branch_name="placeholder",
            run_root=str((self.settings.runs_root / "pending").resolve()),
            preferred_pool=data.resource_profile.pool,
            requires_internet=data.resource_profile.internet,
            required_secrets=data.resource_profile.secrets,
            publish_strategy=data.publish_strategy.value,
            latest_summary=f"Run created and queued for {initial_phase}.",
            next_action=f"Lease {initial_phase} phase",
        )
        self.db.add(run)
        self.db.flush()
        run.branch_name = f"bokkie/run-{run.id[:8]}"
        run.run_root = str((self.settings.runs_root / run.id).resolve())
        self._queue_phase_attempt(run=run, phase_name=initial_phase, payload={})
        self._write_request_artifact(run)
        self._sync_status_artifact(run)
        self._sync_campaign_from_run(run)
        self._add_event(run.id, None, "run.created", run.latest_summary, {"run_id": run.id})
        self.db.commit()
        self.db.refresh(run)
        return run

    def list_runs(self) -> list[Run]:
        statement = (
            select(Run)
            .options(
                selectinload(Run.notes),
                selectinload(Run.phase_attempts),
                selectinload(Run.reviews),
                selectinload(Run.artifacts),
            )
            .order_by(Run.created_at.desc())
        )
        return list(self.db.scalars(statement))

    def list_campaigns(self) -> list[Campaign]:
        statement = (
            select(Campaign)
            .options(
                selectinload(Campaign.runs).selectinload(Run.reviews),
                selectinload(Campaign.runs).selectinload(Run.phase_attempts),
                selectinload(Campaign.drafts),
                selectinload(Campaign.notes),
            )
            .order_by(Campaign.created_at.desc())
        )
        return list(self.db.scalars(statement))

    def list_campaign_drafts(self) -> list[CampaignDraft]:
        statement = select(CampaignDraft).order_by(CampaignDraft.created_at.desc())
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
                selectinload(Run.notes),
                selectinload(Run.phase_attempts),
                selectinload(Run.reviews),
                selectinload(Run.artifacts),
                selectinload(Run.events),
                selectinload(Run.campaign),
            )
        )
        run = self.db.scalars(statement).first()
        if run is None:
            raise OrchestrationError("Run not found")
        return run

    def get_campaign(self, campaign_id: str) -> Campaign:
        statement = (
            select(Campaign)
            .where(Campaign.id == campaign_id)
            .options(
                selectinload(Campaign.project),
                selectinload(Campaign.runs).selectinload(Run.phase_attempts),
                selectinload(Campaign.runs).selectinload(Run.reviews),
                selectinload(Campaign.runs).selectinload(Run.artifacts),
                selectinload(Campaign.runs).selectinload(Run.events),
                selectinload(Campaign.runs).selectinload(Run.notes),
                selectinload(Campaign.drafts),
                selectinload(Campaign.notes),
            )
        )
        campaign = self.db.scalars(statement).first()
        if campaign is None:
            raise OrchestrationError("Campaign not found")
        return campaign

    def get_campaign_draft(self, draft_id: str) -> CampaignDraft:
        draft = self.db.get(CampaignDraft, draft_id)
        if draft is None:
            raise OrchestrationError("Campaign draft not found")
        return draft

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
        phase_attempts = self._claimable_phase_attempts(
            target_phase_attempt_id=target_phase_attempt_id
        )
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
            self._sync_campaign_from_run(run)
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
            self._sync_campaign_from_run(run)
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
        self._sync_campaign_from_run(run)
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
            if run.type in {RunType.EXPERIMENT.value, RunType.INVESTIGATION.value}:
                proposal = ProposeNextPhaseResult.model_validate(review.payload)
                self._complete_experiment_iteration_after_approval(run, proposal, now=now)
            else:
                run.current_stage = RunStage.DONE.value
                run.status = RunStatus.DONE.value
                run.completed_at = now
                if run.publish_strategy == PublishStrategy.PUSH.value and run.project.push_remote:
                    run.next_action = "Run completed; branch is ready to push manually"
                else:
                    run.next_action = "Run completed"
        self._add_event(run.id, None, "review.approved", decision.reason, {"gate": review.gate})
        self._sync_status_artifact(run)
        self._sync_campaign_from_run(run)
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
            if run.type in {RunType.EXPERIMENT.value, RunType.INVESTIGATION.value}:
                run.current_stage = RunStage.DONE.value
                run.status = RunStatus.DONE.value
                run.completed_at = now
                run.next_action = "Operator rejected the proposed next iteration"
                if run.campaign is not None:
                    run.campaign.status = CampaignStatus.PAUSED.value
                    run.campaign.next_action = (
                        "Await operator steering before creating another iteration"
                    )
            else:
                self._queue_phase_attempt(
                    run=run, phase_name=PhaseName.EXECUTE.value, payload=payload
                )
                run.current_stage = RunStage.EXECUTE.value
                run.status = RunStatus.QUEUED.value
                run.next_action = "Re-execute after final review rejection"
        self._add_event(run.id, None, "review.rejected", decision.reason, {"gate": review.gate})
        self._sync_status_artifact(run)
        self._sync_campaign_from_run(run)
        self.db.commit()
        return self.get_run(run.id)

    def pause_run(self, run_id: str) -> Run:
        run = self.get_run(run_id)
        run.status = RunStatus.PAUSED.value
        run.next_action = "Paused by operator"
        self._add_event(run.id, None, "run.paused", run.next_action, {})
        self._sync_status_artifact(run)
        self._sync_campaign_from_run(run)
        self.db.commit()
        return self.get_run(run.id)

    def resume_run(self, run_id: str) -> Run:
        run = self.get_run(run_id)
        pending_review = self._latest_pending_review(run, raise_missing=False)
        run.status = RunStatus.WAITING_REVIEW.value if pending_review else RunStatus.QUEUED.value
        run.next_action = "Resume scheduling"
        self._add_event(run.id, None, "run.resumed", run.next_action, {})
        self._sync_status_artifact(run)
        self._sync_campaign_from_run(run)
        self.db.commit()
        return self.get_run(run.id)

    def steer_run(self, run_id: str, data: OperatorNoteIn) -> Run:
        run = self.get_run(run_id)
        note = OperatorNote(run_id=run.id, note=data.note, created_by=data.created_by)
        self.db.add(note)
        run.next_action = "Apply operator note at the next safe boundary or active steer"
        self._add_event(run.id, None, "run.steered", data.note, {"created_by": data.created_by})
        self._sync_status_artifact(run)
        self._sync_campaign_from_run(run)
        self.db.commit()
        return self.get_run(run.id)

    def steer_campaign(self, campaign_id: str, data: OperatorNoteIn) -> Campaign:
        campaign = self.get_campaign(campaign_id)
        note = OperatorNote(campaign_id=campaign.id, note=data.note, created_by=data.created_by)
        self.db.add(note)
        active_run = self._active_run_for_campaign(campaign)
        if active_run is not None:
            campaign.next_action = "Apply campaign steering to the active run"
            self._add_event(
                active_run.id,
                None,
                "campaign.steered",
                data.note,
                {"created_by": data.created_by, "campaign_id": campaign.id},
            )
        else:
            campaign.next_action = "Queue campaign steering for the next relevant iteration"
        self._sync_campaign_notebook(campaign)
        self.db.commit()
        return self.get_campaign(campaign.id)

    def approve_campaign_gate(self, campaign_id: str, decision: OperatorDecision) -> Campaign:
        campaign = self.get_campaign(campaign_id)
        active_run = self._active_run_for_campaign(campaign)
        if active_run is None:
            raise OrchestrationError("Campaign has no active run awaiting approval")
        self.approve_run(active_run.id, decision)
        return self.get_campaign(campaign.id)

    def reject_campaign_gate(self, campaign_id: str, decision: OperatorDecision) -> Campaign:
        campaign = self.get_campaign(campaign_id)
        active_run = self._active_run_for_campaign(campaign)
        if active_run is None:
            raise OrchestrationError("Campaign has no active run awaiting approval")
        self.reject_run(active_run.id, decision)
        return self.get_campaign(campaign.id)

    def promote_run(self, run_id: str, data: PromoteRunIn) -> Run:
        run = self.get_run(run_id)
        run.preferred_pool = data.pool
        for phase_attempt in run.phase_attempts:
            if phase_attempt.status == PhaseAttemptStatus.QUEUED.value:
                phase_attempt.requested_pool = data.pool
        run.next_action = f"Promoted to pool {data.pool}"
        self._add_event(run.id, None, "run.promoted", run.next_action, {"pool": data.pool})
        self._sync_status_artifact(run)
        self._sync_campaign_from_run(run)
        self.db.commit()
        return self.get_run(run.id)

    def serialize_run(self, run: Run) -> RunRead:
        artifacts = [
            self._artifact_summary(artifact)
            for artifact in sorted(run.artifacts, key=lambda artifact: artifact.created_at)
        ]
        notes = [
            OperatorNoteRead.model_validate(note)
            for note in sorted(run.notes, key=lambda note: note.created_at)
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
            campaign_id=run.campaign_id,
            iteration_no=run.iteration_no,
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
            notes=notes,
        )

    def serialize_worker(self, worker: Worker) -> WorkerRead:
        return WorkerRead.model_validate(worker)

    def serialize_campaign_draft(self, draft: CampaignDraft) -> CampaignDraftRead:
        return CampaignDraftRead(
            id=draft.id,
            project_id=draft.project_id,
            campaign_id=draft.campaign_id,
            operator_prompt=draft.operator_prompt,
            status=draft.status,
            draft=CampaignDraftPayload.model_validate(draft.draft_json),
            decision_reason=draft.decision_reason,
            decided_by=draft.decided_by,
            decided_at=draft.decided_at,
            created_at=draft.created_at,
            updated_at=draft.updated_at,
        )

    def serialize_campaign(self, campaign: Campaign) -> CampaignRead:
        runs = [
            self.serialize_run(run)
            for run in sorted(
                campaign.runs,
                key=lambda run: ((run.iteration_no or 0), run.created_at),
            )
        ]
        drafts = [
            self.serialize_campaign_draft(draft)
            for draft in sorted(campaign.drafts, key=lambda draft: draft.created_at)
        ]
        notes = [
            OperatorNoteRead.model_validate(note)
            for note in sorted(campaign.notes, key=lambda note: note.created_at)
        ]
        active_run = self._active_run_for_campaign(campaign)
        return CampaignRead(
            id=campaign.id,
            project_id=campaign.project_id,
            title=campaign.title,
            objective=campaign.objective,
            status=campaign.status,
            campaign_type=campaign.campaign_type,
            task_name=campaign.task_name,
            preferred_pool=campaign.preferred_pool,
            requires_internet=campaign.requires_internet,
            budget_json=campaign.budget_json,
            continuation_policy_json=campaign.continuation_policy_json,
            approval_gates_json=campaign.approval_gates_json,
            latest_summary=campaign.latest_summary,
            next_action=campaign.next_action,
            current_iteration_no=campaign.current_iteration_no,
            notebook_path=campaign.notebook_path,
            artifact_root=campaign.artifact_root,
            created_at=campaign.created_at,
            updated_at=campaign.updated_at,
            active_run_id=active_run.id if active_run else None,
            runs=runs,
            drafts=drafts,
            notes=notes,
            files=self._campaign_file_links(campaign),
        )

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
            if run.type in {RunType.EXPERIMENT.value, RunType.INVESTIGATION.value}:
                self._queue_phase_attempt(
                    run=run,
                    phase_name=PhaseName.EXECUTE.value,
                    payload={"source_phase_attempt_id": phase_attempt.id},
                )
                run.current_stage = RunStage.EXECUTE.value
                run.status = RunStatus.QUEUED.value
                run.next_action = "Lease execute phase"
            else:
                run.current_stage = RunStage.PLAN_REVIEW.value
                run.status = RunStatus.QUEUED.value
                run.next_action = "Lease plan review phase"
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
            if run.type in {RunType.EXPERIMENT.value, RunType.INVESTIGATION.value}:
                self._queue_phase_attempt(
                    run=run,
                    phase_name=PhaseName.ANALYZE.value,
                    payload={"source_phase_attempt_id": phase_attempt.id},
                )
                run.current_stage = RunStage.ANALYZE.value
                run.status = RunStatus.QUEUED.value
                run.next_action = "Lease analyze phase"
            else:
                self._queue_phase_attempt(
                    run=run,
                    phase_name=PhaseName.FINAL_REVIEW.value,
                    payload={"source_phase_attempt_id": phase_attempt.id},
                )
                run.current_stage = RunStage.FINAL_REVIEW.value
                run.status = RunStatus.QUEUED.value
                run.next_action = "Lease final review phase"
        elif phase_name == PhaseName.ANALYZE.value:
            result = AnalyzePhaseResult.model_validate(result_payload)
            self._write_bundle_artifact(
                run,
                phase_attempt,
                ArtifactKind.ANALYSIS_JSON.value,
                "analysis/summary.json",
                json.dumps(result.model_dump(), indent=2).encode(),
                "application/json",
            )
            self._write_bundle_artifact(
                run,
                phase_attempt,
                ArtifactKind.PROPOSAL.value,
                "analysis/report.md",
                result.report_md.encode(),
                "text/markdown",
            )
            self._queue_phase_attempt(
                run=run,
                phase_name=PhaseName.PROPOSE_NEXT.value,
                payload={"source_phase_attempt_id": phase_attempt.id},
            )
            run.current_stage = RunStage.PROPOSE_NEXT.value
            run.status = RunStatus.QUEUED.value
            run.next_action = "Lease propose-next phase"
        elif phase_name == PhaseName.PROPOSE_NEXT.value:
            result = ProposeNextPhaseResult.model_validate(result_payload)
            self._write_bundle_artifact(
                run,
                phase_attempt,
                ArtifactKind.PROPOSE_NEXT_JSON.value,
                "analysis/propose_next.json",
                json.dumps(result.model_dump(mode="json"), indent=2).encode(),
                "application/json",
            )
            if result.should_continue and self._campaign_review_required_for_proposal(run, result):
                self.db.add(
                    Review(
                        run_id=run.id,
                        phase_attempt_id=phase_attempt.id,
                        gate=ReviewGate.FINAL.value,
                        status=ReviewStatus.PENDING.value,
                        summary=result.summary,
                        payload=result.model_dump(mode="json"),
                    )
                )
                run.current_stage = RunStage.PROPOSE_NEXT.value
                run.status = RunStatus.WAITING_REVIEW.value
                run.next_action = (
                    result.approval_reason or "Await approval for the proposed next iteration"
                )
            elif result.should_continue:
                self._complete_experiment_iteration_after_approval(run, result, now=self._now())
            else:
                run.current_stage = RunStage.DONE.value
                run.status = RunStatus.DONE.value
                run.completed_at = self._now()
                run.next_action = result.recommended_action
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
        attempts_for_phase = [
            phase for phase in run.phase_attempts if phase.phase_name == phase_name
        ]
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

    def _initial_phase_name(self, run_type: str) -> str:
        run_type_config = self.repo_config.run_types.get(run_type)
        if run_type_config and run_type_config.phases:
            return run_type_config.phases[0]
        return PhaseName.PLAN.value

    def _task_config_for_run(self, run: Run) -> TaskConfig:
        default = TaskConfig(name=run.type, run_type=run.type)
        if run.task_name and run.task_name in self.repo_config.tasks:
            return self.repo_config.tasks[run.task_name]
        if run.type in self.repo_config.tasks:
            return self.repo_config.tasks[run.type]
        return default

    def _phase_index(self, run: Run, phase_name: str) -> int:
        run_type_config = self.repo_config.run_types.get(run.type)
        if run_type_config:
            phases = run_type_config.phases
        elif run.type in {RunType.EXPERIMENT.value, RunType.INVESTIGATION.value}:
            phases = EXPERIMENT_PHASES
        else:
            phases = CHANGE_PHASES
        try:
            return phases.index(phase_name)
        except ValueError as exc:
            raise OrchestrationError(
                f"Phase {phase_name} is not configured for run type {run.type}"
            ) from exc

    def _phase_requires_internet(self, run: Run, phase_name: str) -> bool:
        if phase_name in {
            PhaseName.PLAN.value,
            PhaseName.PLAN_REVIEW.value,
            PhaseName.SPEC_REVIEW.value,
            PhaseName.FINAL_REVIEW.value,
        }:
            return True
        return run.requires_internet and phase_name in {
            PhaseName.SPEC.value,
            PhaseName.EXECUTE.value,
            PhaseName.ANALYZE.value,
            PhaseName.PROPOSE_NEXT.value,
        }

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
        run = self.get_run(run_id)
        names = ["request.md", "status.json"]
        if phase_name in {
            PhaseName.PLAN_REVIEW.value,
            PhaseName.SPEC.value,
            PhaseName.SPEC_REVIEW.value,
            PhaseName.EXECUTE.value,
            PhaseName.VERIFY.value,
            PhaseName.ANALYZE.value,
            PhaseName.PROPOSE_NEXT.value,
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
            PhaseName.ANALYZE.value,
            PhaseName.PROPOSE_NEXT.value,
            PhaseName.FINAL_REVIEW.value,
        }:
            names.extend(["exec/PROGRAM.md", "exec/spec.json", "exec/spec_review.json"])
        if phase_name in {
            PhaseName.VERIFY.value,
            PhaseName.ANALYZE.value,
            PhaseName.PROPOSE_NEXT.value,
            PhaseName.FINAL_REVIEW.value,
        }:
            names.append("verify/results.json")
        if phase_name in {PhaseName.PROPOSE_NEXT.value, PhaseName.FINAL_REVIEW.value}:
            names.extend(
                ["analysis/summary.json", "analysis/report.md", "analysis/propose_next.json"]
            )
        artifacts: dict[str, str] = {}
        for name in names:
            artifact = self._latest_artifact_by_name(run_id, name)
            if artifact is not None:
                artifacts[name] = self._artifact_download_url(artifact.id)
        if run.campaign_id:
            artifacts.update(self._campaign_input_artifacts(run.campaign_id))
        return artifacts

    def _write_request_artifact(self, run: Run) -> None:
        text = (
            f"# Request\n\n"
            f"Objective: {run.objective}\n\n"
            f"Success criteria: {run.success_criteria}\n\n"
            f"Run type: {run.type}\n"
            f"Risk level: {run.risk_level}\n"
            f"Campaign id: {run.campaign_id or 'n/a'}\n"
            f"Iteration: {run.iteration_no or 'n/a'}\n"
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
            "campaign_id": run.campaign_id,
            "iteration_no": run.iteration_no,
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

    def campaign_file_path(self, campaign_id: str, relative_path: str) -> Path:
        campaign = self.get_campaign(campaign_id)
        root = self._campaign_root(campaign)
        candidate = (root / relative_path).resolve()
        if not str(candidate).startswith(str(root.resolve())):
            raise OrchestrationError("Campaign file path escapes campaign root")
        if not candidate.exists():
            raise OrchestrationError("Campaign file not found")
        return candidate

    def _campaign_root(self, campaign: Campaign) -> Path:
        return Path(campaign.artifact_root)

    def _campaign_file_download_url(self, campaign_id: str, relative_path: str) -> str:
        return f"{self.settings.api_base_url.rstrip('/')}/api/campaigns/{campaign_id}/artifacts/{relative_path}"

    def _campaign_file_links(self, campaign: Campaign) -> list[CampaignFileLink]:
        root = self._campaign_root(campaign)
        relative_paths: list[tuple[str, str]] = [
            ("Brief", "brief.md"),
            ("Draft JSON", "setup/draft.json"),
            ("Draft MD", "setup/draft.md"),
            ("Notebook", "NOTEBOOK.md"),
            ("Policy", "policy.json"),
        ]
        for run in sorted(
            campaign.runs, key=lambda item: (item.iteration_no or 0, item.created_at)
        ):
            if run.iteration_no is None:
                continue
            relative_paths.append(
                (
                    f"Iteration {run.iteration_no} Status",
                    f"iterations/{run.iteration_no}/status.json",
                )
            )
            relative_paths.append(
                (
                    f"Iteration {run.iteration_no} Summary",
                    f"iterations/{run.iteration_no}/summary.md",
                )
            )
        links: list[CampaignFileLink] = []
        for label, relative_path in relative_paths:
            if (root / relative_path).exists():
                links.append(
                    CampaignFileLink(
                        label=label,
                        relative_path=relative_path,
                        download_url=self._campaign_file_download_url(campaign.id, relative_path),
                    )
                )
        return links

    def _campaign_input_artifacts(self, campaign_id: str) -> dict[str, str]:
        campaign = self.get_campaign(campaign_id)
        root = self._campaign_root(campaign)
        mappings = {
            "campaign/brief.md": "brief.md",
            "campaign/NOTEBOOK.md": "NOTEBOOK.md",
            "campaign/policy.json": "policy.json",
            "campaign/setup/draft.json": "setup/draft.json",
        }
        artifacts: dict[str, str] = {}
        for materialized_path, relative_path in mappings.items():
            if (root / relative_path).exists():
                artifacts[materialized_path] = self._campaign_file_download_url(
                    campaign.id, relative_path
                )
        return artifacts

    def _write_campaign_file(
        self, campaign: Campaign, relative_path: str, content: str | bytes
    ) -> None:
        path = self._campaign_root(campaign) / relative_path
        path.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(content, bytes):
            path.write_bytes(content)
        else:
            path.write_text(content)

    def _sync_campaign_policy_artifact(self, campaign: Campaign) -> None:
        self._write_campaign_file(
            campaign,
            "policy.json",
            json.dumps(
                {
                    "budget": campaign.budget_json,
                    "continuation_policy": campaign.continuation_policy_json,
                    "approval_gates": campaign.approval_gates_json,
                },
                indent=2,
            ),
        )

    def _write_campaign_setup_files(
        self, campaign: Campaign, draft: CampaignDraft, payload: CampaignDraftPayload
    ) -> None:
        self._campaign_root(campaign).mkdir(parents=True, exist_ok=True)
        self._write_campaign_file(
            campaign,
            "brief.md",
            f"# {campaign.title}\n\n{campaign.objective}\n",
        )
        self._write_campaign_file(
            campaign,
            "setup/draft.json",
            json.dumps(payload.model_dump(mode="json"), indent=2),
        )
        self._write_campaign_file(
            campaign,
            "setup/draft.md",
            self._draft_markdown(draft.operator_prompt, payload),
        )
        self._sync_campaign_policy_artifact(campaign)
        self._sync_campaign_notebook(campaign)

    def _draft_markdown(self, prompt: str, payload: CampaignDraftPayload) -> str:
        rationale_lines = (
            "\n".join(f"- {line}" for line in payload.rationale) or "- No rationale recorded."
        )
        deliverables_lines = (
            "\n".join(f"- {line}" for line in payload.initial_deliverables) or "- None"
        )
        approval_lines = "\n".join(f"- {line}" for line in payload.approval_gates) or "- None"
        return (
            f"# Draft Setup\n\n"
            f"## Operator Prompt\n\n{prompt}\n\n"
            f"## Proposed Campaign\n\n"
            f"- Title: {payload.title}\n"
            f"- Objective: {payload.objective}\n"
            f"- Campaign type: {payload.campaign_type}\n"
            f"- First run type: {payload.first_run_type.value}\n"
            f"- Task preset: {payload.task_name or 'n/a'}\n"
            f"- Preferred pool: {payload.preferred_pool or 'n/a'}\n"
            f"- Requires internet: {payload.requires_internet}\n\n"
            f"## Approval Gates\n\n{approval_lines}\n\n"
            f"## Initial Deliverables\n\n{deliverables_lines}\n\n"
            f"## Why\n\n{rationale_lines}\n"
        )

    def _sync_campaign_from_run(self, run: Run) -> None:
        if not run.campaign_id:
            return
        campaign = run.campaign or self.get_campaign(run.campaign_id)
        campaign.current_iteration_no = max(campaign.current_iteration_no, run.iteration_no or 0)
        campaign.latest_summary = run.latest_summary
        campaign.next_action = run.next_action
        if run.status == RunStatus.FAILED.value:
            campaign.status = CampaignStatus.FAILED.value
        elif run.status == RunStatus.PAUSED.value:
            campaign.status = CampaignStatus.PAUSED.value
        elif run.status == RunStatus.WAITING_REVIEW.value:
            campaign.status = CampaignStatus.WAITING_APPROVAL.value
        elif any(
            child.status in {RunStatus.QUEUED.value, RunStatus.RUNNING.value}
            for child in campaign.runs
        ):
            campaign.status = CampaignStatus.ACTIVE.value
        elif campaign.status not in {
            CampaignStatus.PAUSED.value,
            CampaignStatus.FAILED.value,
            CampaignStatus.WAITING_APPROVAL.value,
        }:
            campaign.status = CampaignStatus.COMPLETED.value
        self._sync_campaign_iteration_files(run)
        self._sync_campaign_policy_artifact(campaign)
        self._sync_campaign_notebook(campaign)

    def _sync_campaign_iteration_files(self, run: Run) -> None:
        if not run.campaign_id or run.iteration_no is None:
            return
        campaign = run.campaign or self.get_campaign(run.campaign_id)
        payload = {
            "campaign_id": campaign.id,
            "run_id": run.id,
            "iteration_no": run.iteration_no,
            "status": run.status,
            "current_stage": run.current_stage,
            "objective": run.objective,
            "success_criteria": run.success_criteria,
            "latest_summary": run.latest_summary,
            "next_action": run.next_action,
            "artifacts": [artifact.name for artifact in run.artifacts],
        }
        self._write_campaign_file(
            campaign,
            f"iterations/{run.iteration_no}/status.json",
            json.dumps(payload, indent=2),
        )
        self._write_campaign_file(
            campaign,
            f"iterations/{run.iteration_no}/summary.md",
            f"# Iteration {run.iteration_no}\n\n"
            f"Run: {run.id}\n\n"
            f"Status: {run.status}\n\n"
            f"Stage: {run.current_stage}\n\n"
            f"Summary: {run.latest_summary or 'n/a'}\n\n"
            f"Next: {run.next_action or 'n/a'}\n",
        )

    def _sync_campaign_notebook(self, campaign: Campaign) -> None:
        recent_runs = sorted(
            campaign.runs, key=lambda item: (item.iteration_no or 0, item.created_at)
        )[-5:]
        recent_notes = sorted(campaign.notes, key=lambda item: item.created_at)[-5:]
        runs_text = (
            "\n".join(
                f"- Iteration {run.iteration_no or '?'} · {run.status} · {run.objective}"
                for run in recent_runs
            )
            or "- No runs yet."
        )
        notes_text = (
            "\n".join(
                f"- {note.created_at.isoformat()} · {note.created_by}: {note.note}"
                for note in recent_notes
            )
            or "- No steering notes."
        )
        self._write_campaign_file(
            campaign,
            "NOTEBOOK.md",
            (
                f"# Campaign Notebook\n\n"
                f"Title: {campaign.title}\n\n"
                f"Objective: {campaign.objective}\n\n"
                f"Status: {campaign.status}\n\n"
                f"Current iteration: {campaign.current_iteration_no}\n\n"
                f"Latest summary: {campaign.latest_summary or 'n/a'}\n\n"
                f"Next action: {campaign.next_action or 'n/a'}\n\n"
                f"## Recent Runs\n\n{runs_text}\n\n"
                f"## Recent Notes\n\n{notes_text}\n"
            ),
        )

    def _active_run_for_campaign(self, campaign: Campaign) -> Run | None:
        active_runs = [
            run
            for run in campaign.runs
            if run.status
            in {
                RunStatus.QUEUED.value,
                RunStatus.RUNNING.value,
                RunStatus.WAITING_REVIEW.value,
                RunStatus.PAUSED.value,
            }
        ]
        if not active_runs:
            return None
        return sorted(active_runs, key=lambda item: (item.iteration_no or 0, item.created_at))[-1]

    def _campaign_review_required_for_proposal(
        self, run: Run, result: ProposeNextPhaseResult
    ) -> bool:
        if run.campaign is None:
            return True
        if result.requires_operator_approval or not result.within_policy:
            return True
        if result.next_iteration is None:
            return True
        policy = ContinuationPolicy.model_validate(run.campaign.continuation_policy_json or {})
        if not policy.auto_continue:
            return True
        budget = run.campaign.budget_json or {}
        next_iteration_no = (run.iteration_no or 0) + 1
        max_iterations = budget.get("max_iterations")
        if max_iterations is not None and next_iteration_no > int(max_iterations):
            return True
        max_active_runs = int(budget.get("max_active_runs", 1))
        active_runs = [
            child
            for child in run.campaign.runs
            if child.id != run.id
            and child.status
            in {RunStatus.QUEUED.value, RunStatus.RUNNING.value, RunStatus.WAITING_REVIEW.value}
        ]
        if len(active_runs) >= max_active_runs:
            return True
        max_total_cost = budget.get("max_total_cost")
        recorded_total_cost = float(budget.get("recorded_total_cost") or 0.0)
        if (
            max_total_cost is not None
            and result.estimated_additional_cost is not None
            and recorded_total_cost + result.estimated_additional_cost > float(max_total_cost)
        ):
            return True
        current_data_family = run.campaign.continuation_policy_json.get("current_data_family")
        if (
            policy.pause_on_data_family_change
            and current_data_family
            and result.next_iteration.data_family
            and result.next_iteration.data_family != current_data_family
        ):
            return True
        if (
            policy.pause_on_executor_change
            and result.next_iteration.preferred_pool
            and run.campaign.preferred_pool
            and result.next_iteration.preferred_pool != run.campaign.preferred_pool
        ):
            return True
        return (
            policy.pause_on_special_resources
            and result.next_iteration.preferred_pool
            and any(token in result.next_iteration.preferred_pool for token in ["gpu", "cloud"])
        )

    def _complete_experiment_iteration_after_approval(
        self, run: Run, proposal: ProposeNextPhaseResult, *, now: datetime
    ) -> None:
        run.current_stage = RunStage.DONE.value
        run.status = RunStatus.DONE.value
        run.completed_at = now
        if proposal.should_continue and proposal.next_iteration and run.campaign is not None:
            next_run = self._create_followup_run_from_proposal(
                run.campaign, run, proposal.next_iteration
            )
            run.next_action = f"Queued iteration {next_run.iteration_no}"
            run.campaign.status = CampaignStatus.ACTIVE.value
            run.campaign.next_action = next_run.next_action
        else:
            run.next_action = proposal.recommended_action
            if run.campaign is not None:
                run.campaign.status = CampaignStatus.COMPLETED.value
                run.campaign.next_action = proposal.recommended_action

    def _create_followup_run_from_proposal(
        self, campaign: Campaign, current_run: Run, proposal: ProposedIteration
    ) -> Run:
        if proposal.data_family:
            campaign.continuation_policy_json = {
                **campaign.continuation_policy_json,
                "current_data_family": proposal.data_family,
            }
        if proposal.research_branch:
            campaign.continuation_policy_json = {
                **campaign.continuation_policy_json,
                "current_research_branch": proposal.research_branch,
            }
        run = self.create_run(
            RunCreate(
                project_id=campaign.project_id,
                campaign_id=campaign.id,
                iteration_no=(current_run.iteration_no or campaign.current_iteration_no or 0) + 1,
                type=proposal.run_type,
                task_name=proposal.task_name or campaign.task_name,
                objective=proposal.objective,
                success_criteria=proposal.success_criteria,
                resource_profile={
                    "pool": proposal.preferred_pool or campaign.preferred_pool,
                    "internet": proposal.requires_internet or campaign.requires_internet,
                    "secrets": [],
                },
            )
        )
        campaign.current_iteration_no = run.iteration_no or campaign.current_iteration_no
        campaign.latest_summary = current_run.latest_summary
        return run

    def _infer_project(self, prompt: str, explicit_project_id: str | None = None) -> Project | None:
        if explicit_project_id:
            return self.get_project(explicit_project_id)
        projects = self.list_projects()
        if not projects:
            return None
        prompt_lc = prompt.lower()
        for project in projects:
            if project.slug.lower() in prompt_lc or project.name.lower() in prompt_lc:
                return project
        if len(projects) == 1:
            return projects[0]
        return None

    def _build_campaign_draft_payload(
        self, prompt: str, project: Project | None
    ) -> CampaignDraftPayload:
        prompt_lc = prompt.lower()
        is_research = any(
            token in prompt_lc
            for token in ["research", "experiment", "study", "investigation", "event", "analysis"]
        )
        auto_continue = any(
            token in prompt_lc
            for token in ["autonomous", "continue", "auto-continue", "keep it autonomous"]
        )
        money_match = re.search(r"\$(\d+(?:\.\d+)?)", prompt)
        max_iterations_match = re.search(r"(\d+)\s+iterations?", prompt_lc)
        preferred_pool = None
        preferred_executor_labels: list[str] = []
        if "gpu" in prompt_lc:
            preferred_pool = "gpu-3090"
            preferred_executor_labels.append("gpu")
        elif "devbox" in prompt_lc:
            preferred_pool = "cpu-large"
            preferred_executor_labels.append("highmem")
        elif "cpu-large" in prompt_lc:
            preferred_pool = "cpu-large"
        requires_internet = is_research or "internet" in prompt_lc or "web" in prompt_lc
        task_name = None
        if is_research:
            if "research_iteration" in self.repo_config.tasks:
                task_name = "research_iteration"
            elif "autoresearch" in self.repo_config.tasks:
                task_name = "autoresearch"
        if task_name is None and "change" in self.repo_config.tasks:
            task_name = "change"
        project_label = project.name if project else "Unknown project"
        objective = prompt.strip()
        title = objective.split(".")[0][:120] or f"{project_label} campaign"
        rationale = []
        if project is not None:
            rationale.append(f"Mapped prompt to project {project.slug}.")
        else:
            rationale.append(
                "Could not resolve a project automatically; operator should confirm before approval."
            )
        rationale.append(
            "Chose experiment-style iteration because the request reads like ongoing research/development work."
            if is_research
            else "Chose change-style iteration because the request reads like bounded implementation work."
        )
        if preferred_pool:
            rationale.append(
                f"Preferred pool set to {preferred_pool} from executor/host hints in the prompt."
            )
        if money_match:
            rationale.append(f"Budget cap inferred from prompt as ${money_match.group(1)}.")
        first_run_type = RunType.EXPERIMENT if is_research else RunType.CHANGE
        first_run_objective = (
            f"{objective}\n\nDeliver one bounded iteration, capture results, and recommend the next step."
            if is_research
            else objective
        )
        first_run_success = (
            "Produce artifacts for plan, execution, verification, analysis, and a bounded proposed next iteration."
            if is_research
            else "Deliver the requested change with verification artifacts."
        )
        return CampaignDraftPayload(
            inferred_project_id=project.id if project else None,
            inferred_project_slug=project.slug if project else None,
            inferred_project_name=project.name if project else None,
            title=title,
            objective=objective,
            campaign_type="research" if is_research else "development",
            first_run_type=first_run_type,
            task_name=task_name,
            preferred_pool=preferred_pool,
            preferred_executor_labels=preferred_executor_labels,
            requires_internet=requires_internet,
            budget={
                "max_iterations": int(max_iterations_match.group(1))
                if max_iterations_match
                else (3 if auto_continue else 1),
                "max_total_cost": float(money_match.group(1)) if money_match else None,
                "max_active_runs": 1,
            },
            continuation_policy={
                "auto_continue": auto_continue,
                "require_approval_for": [
                    "changing data family or research branch",
                    "changing executor class or preferred pool materially",
                    "cloud spend or special resources",
                    "anything outside the approved continuation policy",
                ],
            },
            approval_gates=[
                "draft approval before creating the campaign",
                "policy boundary crossings",
                "changing data family or research branch",
                "material executor or spend changes",
            ],
            initial_deliverables=[
                "plan/proposal.md",
                "exec/checkpoints/<phase-id>.json",
                "verify/results.json",
                "analysis/report.md",
                "analysis/propose_next.json",
            ],
            rationale=rationale,
            first_run_objective=first_run_objective,
            first_run_success_criteria=first_run_success,
        )

    def _apply_draft_overrides(
        self, payload: CampaignDraftPayload, overrides: CampaignDraftApprove
    ) -> CampaignDraftPayload:
        draft = payload.model_copy(deep=True)
        if overrides.project_id:
            draft.inferred_project_id = overrides.project_id
        if overrides.title:
            draft.title = overrides.title
        if overrides.objective:
            draft.objective = overrides.objective
        if overrides.campaign_type:
            draft.campaign_type = overrides.campaign_type
        if overrides.first_run_type is not None:
            draft.first_run_type = overrides.first_run_type
        if overrides.task_name is not None:
            draft.task_name = overrides.task_name
        if overrides.preferred_pool is not None:
            draft.preferred_pool = overrides.preferred_pool
        if overrides.requires_internet is not None:
            draft.requires_internet = overrides.requires_internet
        if overrides.max_iterations is not None:
            draft.budget.max_iterations = overrides.max_iterations
        if overrides.max_total_cost is not None:
            draft.budget.max_total_cost = overrides.max_total_cost
        if overrides.auto_continue is not None:
            draft.continuation_policy.auto_continue = overrides.auto_continue
        if overrides.first_run_objective:
            draft.first_run_objective = overrides.first_run_objective
        if overrides.first_run_success_criteria:
            draft.first_run_success_criteria = overrides.first_run_success_criteria
        return draft

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
            artifact.phase_attempt_id = (
                phase_attempt.id if phase_attempt else artifact.phase_attempt_id
            )
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
        scoped_notes = list(run.notes)
        if run.campaign is not None:
            scoped_notes.extend(run.campaign.notes)
        for note in sorted(scoped_notes, key=lambda item: item.created_at):
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
