from __future__ import annotations

from datetime import UTC, datetime, timedelta
from typing import Any

from sqlalchemy import select
from sqlalchemy.orm import Session, selectinload

from ..config import Settings
from ..enums import (
    ArtifactKind,
    PublishStrategy,
    ReviewGate,
    ReviewStatus,
    RunStage,
    RunStatus,
    WorkerState,
    WorkItemKind,
    WorkItemStatus,
)
from ..models import (
    Artifact,
    Event,
    Lease,
    OperatorNote,
    Project,
    Review,
    Run,
    Worker,
    WorkerHeartbeat,
    WorkItem,
)
from ..schemas import (
    ArtifactSummary,
    EventRead,
    ImplementResult,
    OperatorDecision,
    OperatorNoteIn,
    PlanResult,
    ProjectCreate,
    ProjectRead,
    PromoteRunIn,
    ReviewSummary,
    RunCreate,
    RunRead,
    VerifyResult,
    WorkerCapabilities,
    WorkerHeartbeatIn,
    WorkerRead,
    WorkItemCompletionIn,
    WorkItemEventIn,
    WorkItemLeaseResponse,
    WorkItemSummary,
)


class OrchestrationError(RuntimeError):
    pass


class OrchestratorService:
    def __init__(self, db: Session, settings: Settings) -> None:
        self.db = db
        self.settings = settings

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
        run = Run(
            project_id=project.id,
            type=data.type.value,
            objective=data.objective,
            success_criteria=data.success_criteria,
            risk_level=data.risk_level.value,
            budget=data.budget.model_dump(exclude_none=True),
            resource_profile=data.resource_profile.model_dump(exclude_none=True),
            current_stage=RunStage.PLANNING.value,
            status=RunStatus.QUEUED.value,
            base_ref=data.base_ref or project.default_branch,
            branch_name="placeholder",
            preferred_pool=data.resource_profile.pool,
            requires_internet=data.resource_profile.internet,
            required_secrets=data.resource_profile.secrets,
            publish_strategy=data.publish_strategy.value,
            latest_summary="Run created and queued for planning.",
            next_action="Lease planning work item",
        )
        self.db.add(run)
        self.db.flush()
        run.branch_name = f"bokkie/run-{run.id[:8]}"
        self._queue_work_item(
            run=run,
            kind=WorkItemKind.PLAN,
            prompt_template="planner.v1",
            payload={},
        )
        self._add_event(run.id, None, "run.created", run.latest_summary, {"run_id": run.id})
        self.db.commit()
        self.db.refresh(run)
        return run

    def list_runs(self) -> list[Run]:
        statement = (
            select(Run)
            .options(
                selectinload(Run.work_items),
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
                selectinload(Run.work_items),
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

    def claim_work_item(self, worker_id: str) -> WorkItemLeaseResponse:
        worker = self.db.get(Worker, worker_id)
        if worker is None:
            raise OrchestrationError("Worker not found")
        self._expire_leases()
        statement = (
            select(WorkItem)
            .join(Run)
            .where(
                WorkItem.status == WorkItemStatus.QUEUED.value,
                Run.status.in_([RunStatus.QUEUED.value, RunStatus.RUNNING.value]),
            )
            .order_by(WorkItem.created_at, WorkItem.sequence_no)
            .with_for_update(skip_locked=True)
        )
        for item in self.db.scalars(statement):
            run = item.run
            if run.status == RunStatus.PAUSED.value:
                continue
            if not self._worker_matches(worker, run, item):
                continue
            self._apply_pending_notes(run, item)
            now = self._now()
            item.status = WorkItemStatus.RUNNING.value
            item.worker_id = worker.id
            item.started_at = item.started_at or now
            item.lease_expires_at = now + timedelta(seconds=self.settings.lease_ttl_seconds)
            run.current_worker_id = worker.id
            if run.status != RunStatus.PAUSED.value:
                run.status = RunStatus.RUNNING.value
            run.started_at = run.started_at or now
            worker.state = WorkerState.BUSY.value
            worker.current_load = 1
            lease = Lease(
                work_item_id=item.id,
                worker_id=worker.id,
                expires_at=item.lease_expires_at,
            )
            self.db.add(lease)
            self._add_event(
                run.id,
                item.id,
                "lease.acquired",
                "Work item leased",
                {"worker_id": worker.id},
            )
            self.db.commit()
            self.db.refresh(item)
            run = self.get_run(run.id)
            project = self.get_project(run.project_id)
            prior_patch_downloads = [
                self._artifact_download_url(artifact.id)
                for artifact in self._prior_patch_artifacts(run.id, upto_work_item_id=item.id)
            ]
            return WorkItemLeaseResponse(
                leased=True,
                work_item=self._work_item_summary(item),
                run=self.serialize_run(run),
                project=ProjectRead.model_validate(project),
                prior_patch_downloads=prior_patch_downloads,
            )
        self.db.commit()
        return WorkItemLeaseResponse(leased=False)

    def add_event(self, work_item_id: str, data: WorkItemEventIn) -> Event:
        item = self.db.get(WorkItem, work_item_id)
        if item is None:
            raise OrchestrationError("Work item not found")
        event = self._add_event(
            item.run_id,
            work_item_id,
            data.event_type,
            data.summary,
            data.payload,
            data.worker_id,
        )
        self.db.commit()
        self.db.refresh(event)
        return event

    def complete_work_item(self, work_item_id: str, data: WorkItemCompletionIn) -> WorkItem:
        item = self.db.get(WorkItem, work_item_id)
        if item is None:
            raise OrchestrationError("Work item not found")
        run = item.run
        worker = self.db.get(Worker, data.worker_id)
        now = self._now()
        self._release_open_leases(item.id, "completed" if data.success else "failed")
        if worker is not None:
            worker.state = WorkerState.IDLE.value
            worker.current_load = 0
        item.result = data.result
        item.error_text = data.error_text
        item.completed_at = now
        run.current_worker_id = None
        if not data.success:
            item.status = WorkItemStatus.FAILED.value
            run.status = RunStatus.FAILED.value
            run.next_action = "Inspect failed work item"
            self._add_event(
                run.id,
                item.id,
                "work_item.failed",
                data.error_text,
                {"result": data.result},
            )
            self.db.commit()
            return item

        item.status = WorkItemStatus.COMPLETED.value
        summary = data.summary or data.result.get("summary")
        if summary:
            run.latest_summary = summary
        self._add_event(run.id, item.id, "work_item.completed", summary, {"result": data.result})

        if item.kind == WorkItemKind.PLAN.value:
            plan_result = PlanResult.model_validate(data.result)
            run.current_stage = RunStage.REVIEW_GATE_PLAN.value
            run.status = RunStatus.WAITING_REVIEW.value
            run.next_action = "Await operator review of planning output"
            run.blockers = plan_result.blockers
            run.risk_flags = plan_result.risk_flags
            review = Review(
                run_id=run.id,
                gate=ReviewGate.PLAN.value,
                status=ReviewStatus.PENDING.value,
                summary=plan_result.summary,
                payload=plan_result.model_dump(),
            )
            self.db.add(review)
        elif item.kind == WorkItemKind.IMPLEMENT.value:
            implement_result = ImplementResult.model_validate(data.result)
            run.current_stage = RunStage.VERIFY.value
            run.next_action = implement_result.next_action or "Queue verifier"
            if run.status != RunStatus.PAUSED.value:
                run.status = RunStatus.QUEUED.value
            self._queue_work_item(
                run=run,
                kind=WorkItemKind.VERIFY,
                prompt_template="verifier.v1",
                payload={},
                requested_pool=run.preferred_pool,
            )
        elif item.kind == WorkItemKind.VERIFY.value:
            verify_result = VerifyResult.model_validate(data.result)
            run.current_stage = RunStage.REVIEW_GATE_VERIFY.value
            run.status = RunStatus.WAITING_REVIEW.value
            run.latest_verifier_result = verify_result.model_dump(by_alias=True)
            run.next_action = verify_result.next_action
            review = Review(
                run_id=run.id,
                gate=ReviewGate.VERIFY.value,
                status=ReviewStatus.PENDING.value,
                summary=verify_result.summary,
                payload=verify_result.model_dump(by_alias=True),
            )
            self.db.add(review)
        elif item.kind == WorkItemKind.PUBLISH.value:
            run.current_stage = RunStage.PUBLISH.value
            run.status = RunStatus.DONE.value
            run.completed_at = now
            run.next_action = "Run completed"
        self.db.commit()
        self.db.refresh(item)
        return item

    def approve_run(self, run_id: str, decision: OperatorDecision) -> Run:
        run = self.get_run(run_id)
        review = self._latest_pending_review(run)
        now = self._now()
        review.status = ReviewStatus.APPROVED.value
        review.decision_reason = decision.reason
        review.decided_by = decision.actor
        review.decided_at = now
        if review.gate == ReviewGate.PLAN.value:
            plan = PlanResult.model_validate(review.payload)
            run.current_stage = RunStage.WORK_ITEM_GENERATION.value
            work_specs: list[dict[str, Any]]
            if plan.work_items:
                work_specs = [spec.model_dump() for spec in plan.work_items]
            else:
                work_specs = [
                    {
                        "title": "Implement approved plan",
                        "instructions": plan.summary,
                        "requested_pool": run.preferred_pool,
                    }
                ]
            for work in work_specs:
                title = work["title"]
                instructions = work["instructions"]
                requested_pool = work.get("requested_pool")
                self._queue_work_item(
                    run=run,
                    kind=WorkItemKind.IMPLEMENT,
                    prompt_template="executor.v1",
                    payload={"title": title, "instructions": instructions},
                    requested_pool=requested_pool or run.preferred_pool,
                )
            run.current_stage = RunStage.EXECUTE.value
            if run.status != RunStatus.PAUSED.value:
                run.status = RunStatus.QUEUED.value
            run.next_action = "Lease implementation work item"
        elif review.gate == ReviewGate.VERIFY.value:
            if run.publish_strategy == PublishStrategy.PUSH.value and run.project.push_remote:
                self._queue_work_item(
                    run=run,
                    kind=WorkItemKind.PUBLISH,
                    prompt_template="publish.v1",
                    payload={},
                    requested_pool=run.preferred_pool,
                )
                run.current_stage = RunStage.PUBLISH.value
                if run.status != RunStatus.PAUSED.value:
                    run.status = RunStatus.QUEUED.value
                run.next_action = "Lease publish work item"
            else:
                run.current_stage = RunStage.PUBLISH.value
                run.status = RunStatus.DONE.value
                run.completed_at = now
                run.next_action = "Run completed without publish push"
        self._add_event(run.id, None, "review.approved", decision.reason, {"gate": review.gate})
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
        if review.gate == ReviewGate.PLAN.value:
            run.current_stage = RunStage.PLANNING.value
            if run.status != RunStatus.PAUSED.value:
                run.status = RunStatus.QUEUED.value
            run.next_action = "Re-plan after operator rejection"
            self._queue_work_item(
                run=run,
                kind=WorkItemKind.PLAN,
                prompt_template="planner.v1",
                payload={"rejection_reason": decision.reason},
            )
        elif review.gate == ReviewGate.VERIFY.value:
            verifier_findings = review.payload.get("findings", [])
            self._queue_work_item(
                run=run,
                kind=WorkItemKind.IMPLEMENT,
                prompt_template="repair.v1",
                payload={
                    "title": "Address verifier feedback",
                    "instructions": "Fix issues raised during verification.",
                    "verifier_findings": verifier_findings,
                    "rejection_reason": decision.reason,
                },
                requested_pool=run.preferred_pool,
            )
            run.current_stage = RunStage.EXECUTE.value
            if run.status != RunStatus.PAUSED.value:
                run.status = RunStatus.QUEUED.value
            run.next_action = "Lease repair work item"
        self._add_event(run.id, None, "review.rejected", decision.reason, {"gate": review.gate})
        self.db.commit()
        return self.get_run(run.id)

    def pause_run(self, run_id: str) -> Run:
        run = self.get_run(run_id)
        run.status = RunStatus.PAUSED.value
        run.next_action = "Paused by operator"
        self._add_event(run.id, None, "run.paused", run.next_action, {})
        self.db.commit()
        return self.get_run(run.id)

    def resume_run(self, run_id: str) -> Run:
        run = self.get_run(run_id)
        pending_review = self._latest_pending_review(run, raise_missing=False)
        run.status = RunStatus.WAITING_REVIEW.value if pending_review else RunStatus.QUEUED.value
        run.next_action = "Resume scheduling"
        self._add_event(run.id, None, "run.resumed", run.next_action, {})
        self.db.commit()
        return self.get_run(run.id)

    def steer_run(self, run_id: str, data: OperatorNoteIn) -> Run:
        run = self.get_run(run_id)
        note = OperatorNote(run_id=run.id, note=data.note, created_by=data.created_by)
        self.db.add(note)
        run.next_action = "Apply operator note at next safe boundary"
        self._add_event(run.id, None, "run.steered", data.note, {"created_by": data.created_by})
        self.db.commit()
        return self.get_run(run.id)

    def promote_run(self, run_id: str, data: PromoteRunIn) -> Run:
        run = self.get_run(run_id)
        run.preferred_pool = data.pool
        for item in run.work_items:
            if item.status == WorkItemStatus.QUEUED.value:
                item.requested_pool = data.pool
        run.next_action = f"Promoted to pool {data.pool}"
        self._add_event(run.id, None, "run.promoted", run.next_action, {"pool": data.pool})
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
            for event in sorted(run.events, key=lambda event: event.id)[-50:]
        ]
        return RunRead(
            id=run.id,
            project_id=run.project_id,
            type=run.type,
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
            work_items=[
                self._work_item_summary(item)
                for item in sorted(run.work_items, key=lambda item: item.sequence_no)
            ],
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

    def get_artifact(self, artifact_id: str) -> Artifact:
        artifact = self.db.get(Artifact, artifact_id)
        if artifact is None:
            raise OrchestrationError("Artifact not found")
        return artifact

    def create_artifact(
        self,
        run_id: str,
        work_item_id: str | None,
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
            work_item_id=work_item_id,
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

    def _work_item_summary(self, item: WorkItem) -> WorkItemSummary:
        return WorkItemSummary.model_validate(item)

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

    def _add_event(
        self,
        run_id: str,
        work_item_id: str | None,
        event_type: str,
        summary: str | None,
        payload: dict[str, Any],
        worker_id: str | None = None,
    ) -> Event:
        event = Event(
            run_id=run_id,
            work_item_id=work_item_id,
            worker_id=worker_id,
            event_type=event_type,
            summary=summary,
            payload=payload,
        )
        self.db.add(event)
        return event

    def _queue_work_item(
        self,
        run: Run,
        kind: WorkItemKind,
        prompt_template: str,
        payload: dict[str, Any],
        requested_pool: str | None = None,
    ) -> WorkItem:
        sequence_no = len(run.work_items) + 1
        item = WorkItem(
            run_id=run.id,
            sequence_no=sequence_no,
            kind=kind.value,
            status=WorkItemStatus.QUEUED.value,
            prompt_template=prompt_template,
            requested_pool=requested_pool,
            requires_internet=run.requires_internet,
            required_secrets=run.required_secrets,
            timeout_seconds=1800,
            retry_limit=1,
            base_ref=run.base_ref,
            branch_name=run.branch_name,
            payload=payload,
        )
        self.db.add(item)
        run.work_items.append(item)
        return item

    def _latest_pending_review(self, run: Run, raise_missing: bool = True) -> Review | None:
        for review in sorted(run.reviews, key=lambda review: review.created_at, reverse=True):
            if review.status == ReviewStatus.PENDING.value:
                return review
        if raise_missing:
            raise OrchestrationError("No pending review for run")
        return None

    def _artifact_download_url(self, artifact_id: str) -> str:
        return f"{self.settings.api_base_url}/api/artifacts/{artifact_id}/download"

    def _prior_patch_artifacts(self, run_id: str, upto_work_item_id: str) -> list[Artifact]:
        work_item = self.db.get(WorkItem, upto_work_item_id)
        if work_item is None:
            return []
        statement = (
            select(Artifact)
            .join(WorkItem, Artifact.work_item_id == WorkItem.id)
            .where(
                Artifact.run_id == run_id,
                Artifact.kind == ArtifactKind.PATCH.value,
                WorkItem.sequence_no < work_item.sequence_no,
            )
            .order_by(WorkItem.sequence_no)
        )
        return list(self.db.scalars(statement))

    def _apply_pending_notes(self, run: Run, item: WorkItem) -> None:
        notes = (
            self.db.query(OperatorNote)
            .filter(OperatorNote.run_id == run.id, OperatorNote.applied_at.is_(None))
            .order_by(OperatorNote.created_at)
            .all()
        )
        if not notes:
            return
        payload_notes = list(item.payload.get("operator_notes", []))
        payload_notes.extend(note.note for note in notes)
        item.payload = {**item.payload, "operator_notes": payload_notes}
        now = self._now()
        for note in notes:
            note.applied_at = now

    def _expire_leases(self) -> None:
        now = self._now()
        statement = select(WorkItem).where(
            WorkItem.status == WorkItemStatus.RUNNING.value,
            WorkItem.lease_expires_at.is_not(None),
            WorkItem.lease_expires_at < now,
        )
        for item in self.db.scalars(statement):
            run = item.run
            self._release_open_leases(item.id, "expired")
            if item.retry_count < item.retry_limit:
                item.retry_count += 1
                item.status = WorkItemStatus.QUEUED.value
                item.worker_id = None
                item.lease_expires_at = None
                run.status = RunStatus.QUEUED.value
                self._add_event(
                    run.id,
                    item.id,
                    "lease.expired",
                    "Lease expired and item re-queued",
                    {},
                )
            else:
                item.status = WorkItemStatus.FAILED.value
                run.status = RunStatus.FAILED.value
                run.next_action = "Exceeded retry budget"

    def _release_open_leases(self, work_item_id: str, reason: str) -> None:
        now = self._now()
        statement = select(Lease).where(
            Lease.work_item_id == work_item_id,
            Lease.released_at.is_(None),
        )
        for lease in self.db.scalars(statement):
            lease.released_at = now
            lease.release_reason = reason

    def _worker_matches(self, worker: Worker, run: Run, item: WorkItem) -> bool:
        requested_pool = item.requested_pool or run.preferred_pool
        if requested_pool and requested_pool not in worker.pools:
            return False
        if item.requires_internet and "internet" not in worker.labels:
            return False
        needed_secrets = set(item.required_secrets or [])
        return needed_secrets.issubset(set(worker.secrets or []))

    def _now(self) -> datetime:
        return datetime.now(UTC)
