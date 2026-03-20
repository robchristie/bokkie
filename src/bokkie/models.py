from __future__ import annotations

from datetime import datetime
from uuid import uuid4

from sqlalchemy import JSON, DateTime, ForeignKey, Integer, String, Text, UniqueConstraint, func
from sqlalchemy.orm import Mapped, mapped_column, relationship

from .db import Base
from .enums import (
    ArtifactKind,
    PhaseAttemptStatus,
    PhaseName,
    PhaseRole,
    PublishStrategy,
    ReviewGate,
    ReviewStatus,
    RiskLevel,
    RunStage,
    RunStatus,
    RunType,
    WorkerState,
)


def new_id() -> str:
    return uuid4().hex


class TimestampMixin:
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), server_default=func.now())
    updated_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), server_default=func.now(), onupdate=func.now()
    )


class Project(Base, TimestampMixin):
    __tablename__ = "projects"

    id: Mapped[str] = mapped_column(String(32), primary_key=True, default=new_id)
    slug: Mapped[str] = mapped_column(String(120), unique=True, index=True)
    name: Mapped[str] = mapped_column(String(200))
    repo_url: Mapped[str] = mapped_column(Text)
    default_branch: Mapped[str] = mapped_column(String(120), default="main")
    push_remote: Mapped[str | None] = mapped_column(String(120), nullable=True)
    allowed_pools: Mapped[list[str]] = mapped_column(JSON, default=list)
    required_secrets: Mapped[list[str]] = mapped_column(JSON, default=list)
    command_profiles: Mapped[dict[str, object]] = mapped_column(JSON, default=dict)
    settings: Mapped[dict[str, object]] = mapped_column(JSON, default=dict)

    campaigns: Mapped[list[Campaign]] = relationship(back_populates="project")
    runs: Mapped[list[Run]] = relationship(back_populates="project")


class Worker(Base, TimestampMixin):
    __tablename__ = "workers"

    id: Mapped[str] = mapped_column(String(120), primary_key=True)
    host: Mapped[str] = mapped_column(String(200))
    pools: Mapped[list[str]] = mapped_column(JSON, default=list)
    labels: Mapped[list[str]] = mapped_column(JSON, default=list)
    secrets: Mapped[list[str]] = mapped_column(JSON, default=list)
    cpu_cores: Mapped[int | None] = mapped_column(Integer, nullable=True)
    ram_gb: Mapped[int | None] = mapped_column(Integer, nullable=True)
    gpu_model: Mapped[str | None] = mapped_column(String(120), nullable=True)
    gpu_vram_gb: Mapped[int | None] = mapped_column(Integer, nullable=True)
    state: Mapped[str] = mapped_column(String(32), default=WorkerState.IDLE.value)
    current_load: Mapped[int] = mapped_column(Integer, default=0)
    metadata_json: Mapped[dict[str, object]] = mapped_column("metadata", JSON, default=dict)
    last_seen_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), server_default=func.now()
    )

    runs: Mapped[list[Run]] = relationship(back_populates="current_worker")
    heartbeats: Mapped[list[WorkerHeartbeat]] = relationship(back_populates="worker")
    phase_attempts: Mapped[list[PhaseAttempt]] = relationship(back_populates="worker")


class Campaign(Base, TimestampMixin):
    __tablename__ = "campaigns"

    id: Mapped[str] = mapped_column(String(32), primary_key=True, default=new_id)
    project_id: Mapped[str] = mapped_column(ForeignKey("projects.id"), index=True)
    title: Mapped[str] = mapped_column(String(240))
    objective: Mapped[str] = mapped_column(Text)
    status: Mapped[str] = mapped_column(String(32), index=True)
    campaign_type: Mapped[str] = mapped_column(String(64), default="campaign")
    task_name: Mapped[str | None] = mapped_column(String(120), nullable=True)
    preferred_pool: Mapped[str | None] = mapped_column(String(120), nullable=True)
    requires_internet: Mapped[bool] = mapped_column(default=False)
    budget_json: Mapped[dict[str, object]] = mapped_column(JSON, default=dict)
    continuation_policy_json: Mapped[dict[str, object]] = mapped_column(JSON, default=dict)
    approval_gates_json: Mapped[list[str]] = mapped_column(JSON, default=list)
    latest_summary: Mapped[str | None] = mapped_column(Text, nullable=True)
    next_action: Mapped[str | None] = mapped_column(Text, nullable=True)
    current_iteration_no: Mapped[int] = mapped_column(Integer, default=0)
    notebook_path: Mapped[str] = mapped_column(Text)
    artifact_root: Mapped[str] = mapped_column(Text)

    project: Mapped[Project] = relationship(back_populates="campaigns")
    runs: Mapped[list[Run]] = relationship(back_populates="campaign")
    drafts: Mapped[list[CampaignDraft]] = relationship(back_populates="campaign")
    notes: Mapped[list[OperatorNote]] = relationship(back_populates="campaign")


class CampaignDraft(Base, TimestampMixin):
    __tablename__ = "campaign_drafts"

    id: Mapped[str] = mapped_column(String(32), primary_key=True, default=new_id)
    project_id: Mapped[str | None] = mapped_column(
        ForeignKey("projects.id"), nullable=True, index=True
    )
    campaign_id: Mapped[str | None] = mapped_column(
        ForeignKey("campaigns.id"), nullable=True, index=True
    )
    operator_prompt: Mapped[str] = mapped_column(Text)
    status: Mapped[str] = mapped_column(String(32), default="draft", index=True)
    draft_json: Mapped[dict[str, object]] = mapped_column(JSON, default=dict)
    decision_reason: Mapped[str | None] = mapped_column(Text, nullable=True)
    decided_by: Mapped[str | None] = mapped_column(String(120), nullable=True)
    decided_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)

    project: Mapped[Project | None] = relationship()
    campaign: Mapped[Campaign | None] = relationship(back_populates="drafts")


class Run(Base, TimestampMixin):
    __tablename__ = "runs"

    id: Mapped[str] = mapped_column(String(32), primary_key=True, default=new_id)
    project_id: Mapped[str] = mapped_column(ForeignKey("projects.id"), index=True)
    campaign_id: Mapped[str | None] = mapped_column(
        ForeignKey("campaigns.id"), nullable=True, index=True
    )
    iteration_no: Mapped[int | None] = mapped_column(Integer, nullable=True)
    type: Mapped[str] = mapped_column(String(32), default=RunType.CHANGE.value)
    task_name: Mapped[str | None] = mapped_column(String(120), nullable=True)
    objective: Mapped[str] = mapped_column(Text)
    success_criteria: Mapped[str] = mapped_column(Text)
    risk_level: Mapped[str] = mapped_column(String(16), default=RiskLevel.MEDIUM.value)
    budget: Mapped[dict[str, object]] = mapped_column(JSON, default=dict)
    resource_profile: Mapped[dict[str, object]] = mapped_column(JSON, default=dict)
    current_stage: Mapped[str] = mapped_column(String(40), default=RunStage.INTAKE.value)
    current_session_id: Mapped[str | None] = mapped_column(String(120), nullable=True)
    status: Mapped[str] = mapped_column(String(32), default=RunStatus.QUEUED.value, index=True)
    base_ref: Mapped[str | None] = mapped_column(String(120), nullable=True)
    branch_name: Mapped[str] = mapped_column(String(160))
    run_root: Mapped[str] = mapped_column(Text)
    latest_summary: Mapped[str | None] = mapped_column(Text, nullable=True)
    current_worker_id: Mapped[str | None] = mapped_column(ForeignKey("workers.id"), nullable=True)
    latest_verifier_result: Mapped[dict[str, object] | None] = mapped_column(JSON, nullable=True)
    next_action: Mapped[str | None] = mapped_column(Text, nullable=True)
    blockers: Mapped[list[str]] = mapped_column(JSON, default=list)
    risk_flags: Mapped[list[str]] = mapped_column(JSON, default=list)
    preferred_pool: Mapped[str | None] = mapped_column(String(120), nullable=True)
    requires_internet: Mapped[bool] = mapped_column(default=False)
    required_secrets: Mapped[list[str]] = mapped_column(JSON, default=list)
    publish_strategy: Mapped[str] = mapped_column(String(32), default=PublishStrategy.NONE.value)
    started_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    completed_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)

    project: Mapped[Project] = relationship(back_populates="runs")
    campaign: Mapped[Campaign | None] = relationship(back_populates="runs")
    current_worker: Mapped[Worker | None] = relationship(back_populates="runs")
    phase_attempts: Mapped[list[PhaseAttempt]] = relationship(back_populates="run")
    reviews: Mapped[list[Review]] = relationship(back_populates="run")
    artifacts: Mapped[list[Artifact]] = relationship(back_populates="run")
    events: Mapped[list[Event]] = relationship(back_populates="run")
    notes: Mapped[list[OperatorNote]] = relationship(back_populates="run")
    experiment_results: Mapped[list[ExperimentResult]] = relationship(back_populates="run")


class PhaseAttempt(Base, TimestampMixin):
    __tablename__ = "phase_attempts"
    __table_args__ = (
        UniqueConstraint("run_id", "phase_name", "attempt_no", name="uq_phase_attempt"),
    )

    id: Mapped[str] = mapped_column(String(32), primary_key=True, default=new_id)
    run_id: Mapped[str] = mapped_column(ForeignKey("runs.id"), index=True)
    phase_name: Mapped[str] = mapped_column(String(32), default=PhaseName.PLAN.value, index=True)
    phase_index: Mapped[int] = mapped_column(Integer)
    attempt_no: Mapped[int] = mapped_column(Integer, default=1)
    role: Mapped[str] = mapped_column(String(32), default=PhaseRole.PLANNER.value)
    status: Mapped[str] = mapped_column(
        String(32), default=PhaseAttemptStatus.QUEUED.value, index=True
    )
    requested_pool: Mapped[str | None] = mapped_column(String(120), nullable=True)
    assigned_executor_name: Mapped[str | None] = mapped_column(String(120), nullable=True)
    required_labels: Mapped[list[str]] = mapped_column(JSON, default=list)
    requires_internet: Mapped[bool] = mapped_column(default=False)
    required_secrets: Mapped[list[str]] = mapped_column(JSON, default=list)
    timeout_seconds: Mapped[int] = mapped_column(Integer, default=1800)
    retry_limit: Mapped[int] = mapped_column(Integer, default=1)
    retry_count: Mapped[int] = mapped_column(Integer, default=0)
    dispatch_attempts: Mapped[int] = mapped_column(Integer, default=0)
    last_dispatch_at: Mapped[datetime | None] = mapped_column(
        DateTime(timezone=True), nullable=True
    )
    thread_id: Mapped[str | None] = mapped_column(String(120), nullable=True)
    last_turn_id: Mapped[str | None] = mapped_column(String(120), nullable=True)
    worker_id: Mapped[str | None] = mapped_column(ForeignKey("workers.id"), nullable=True)
    branch_name: Mapped[str | None] = mapped_column(String(160), nullable=True)
    worktree_path: Mapped[str | None] = mapped_column(Text, nullable=True)
    started_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    completed_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    input_artifact_refs: Mapped[dict[str, str]] = mapped_column(JSON, default=dict)
    payload: Mapped[dict[str, object]] = mapped_column(JSON, default=dict)
    result: Mapped[dict[str, object] | None] = mapped_column(JSON, nullable=True)
    error_text: Mapped[str | None] = mapped_column(Text, nullable=True)

    run: Mapped[Run] = relationship(back_populates="phase_attempts")
    worker: Mapped[Worker | None] = relationship(back_populates="phase_attempts")
    events: Mapped[list[Event]] = relationship(back_populates="phase_attempt")
    artifacts: Mapped[list[Artifact]] = relationship(back_populates="phase_attempt")
    leases: Mapped[list[Lease]] = relationship(back_populates="phase_attempt")
    reviews: Mapped[list[Review]] = relationship(back_populates="phase_attempt")


class Review(Base, TimestampMixin):
    __tablename__ = "reviews"

    id: Mapped[str] = mapped_column(String(32), primary_key=True, default=new_id)
    run_id: Mapped[str] = mapped_column(ForeignKey("runs.id"), index=True)
    phase_attempt_id: Mapped[str | None] = mapped_column(
        ForeignKey("phase_attempts.id"), nullable=True
    )
    gate: Mapped[str] = mapped_column(String(32), default=ReviewGate.PLAN.value)
    status: Mapped[str] = mapped_column(String(32), default=ReviewStatus.PENDING.value, index=True)
    summary: Mapped[str | None] = mapped_column(Text, nullable=True)
    payload: Mapped[dict[str, object]] = mapped_column(JSON, default=dict)
    decision_reason: Mapped[str | None] = mapped_column(Text, nullable=True)
    decided_by: Mapped[str | None] = mapped_column(String(120), nullable=True)
    decided_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)

    run: Mapped[Run] = relationship(back_populates="reviews")
    phase_attempt: Mapped[PhaseAttempt | None] = relationship(back_populates="reviews")


class Artifact(Base, TimestampMixin):
    __tablename__ = "artifacts"

    id: Mapped[str] = mapped_column(String(32), primary_key=True, default=new_id)
    run_id: Mapped[str] = mapped_column(ForeignKey("runs.id"), index=True)
    phase_attempt_id: Mapped[str | None] = mapped_column(
        ForeignKey("phase_attempts.id"), nullable=True
    )
    kind: Mapped[str] = mapped_column(String(32), default=ArtifactKind.LOG.value)
    name: Mapped[str] = mapped_column(String(200))
    storage_path: Mapped[str] = mapped_column(Text)
    content_type: Mapped[str] = mapped_column(String(120), default="application/octet-stream")
    sha256: Mapped[str] = mapped_column(String(64))
    size_bytes: Mapped[int] = mapped_column(Integer)
    metadata_json: Mapped[dict[str, object]] = mapped_column("metadata", JSON, default=dict)

    run: Mapped[Run] = relationship(back_populates="artifacts")
    phase_attempt: Mapped[PhaseAttempt | None] = relationship(back_populates="artifacts")


class Event(Base):
    __tablename__ = "events"

    id: Mapped[int] = mapped_column(Integer, primary_key=True, autoincrement=True)
    run_id: Mapped[str] = mapped_column(ForeignKey("runs.id"), index=True)
    phase_attempt_id: Mapped[str | None] = mapped_column(
        ForeignKey("phase_attempts.id"), nullable=True
    )
    worker_id: Mapped[str | None] = mapped_column(ForeignKey("workers.id"), nullable=True)
    event_type: Mapped[str] = mapped_column(String(120), index=True)
    summary: Mapped[str | None] = mapped_column(Text, nullable=True)
    payload: Mapped[dict[str, object]] = mapped_column(JSON, default=dict)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), server_default=func.now())

    run: Mapped[Run] = relationship(back_populates="events")
    phase_attempt: Mapped[PhaseAttempt | None] = relationship(back_populates="events")


class WorkerHeartbeat(Base):
    __tablename__ = "worker_heartbeats"

    id: Mapped[int] = mapped_column(Integer, primary_key=True, autoincrement=True)
    worker_id: Mapped[str] = mapped_column(ForeignKey("workers.id"), index=True)
    observed_load: Mapped[int] = mapped_column(Integer, default=0)
    payload: Mapped[dict[str, object]] = mapped_column(JSON, default=dict)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), server_default=func.now())

    worker: Mapped[Worker] = relationship(back_populates="heartbeats")


class Lease(Base):
    __tablename__ = "leases"

    id: Mapped[str] = mapped_column(String(32), primary_key=True, default=new_id)
    phase_attempt_id: Mapped[str] = mapped_column(ForeignKey("phase_attempts.id"), index=True)
    worker_id: Mapped[str] = mapped_column(ForeignKey("workers.id"), index=True)
    acquired_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), server_default=func.now()
    )
    expires_at: Mapped[datetime] = mapped_column(DateTime(timezone=True))
    released_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    release_reason: Mapped[str | None] = mapped_column(Text, nullable=True)

    phase_attempt: Mapped[PhaseAttempt] = relationship(back_populates="leases")


class OperatorNote(Base):
    __tablename__ = "operator_notes"

    id: Mapped[str] = mapped_column(String(32), primary_key=True, default=new_id)
    run_id: Mapped[str | None] = mapped_column(ForeignKey("runs.id"), nullable=True, index=True)
    campaign_id: Mapped[str | None] = mapped_column(
        ForeignKey("campaigns.id"), nullable=True, index=True
    )
    note: Mapped[str] = mapped_column(Text)
    created_by: Mapped[str] = mapped_column(String(120), default="operator")
    applied_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), server_default=func.now())

    run: Mapped[Run | None] = relationship(back_populates="notes")
    campaign: Mapped[Campaign | None] = relationship(back_populates="notes")


class ExperimentResult(Base):
    __tablename__ = "experiment_results"

    id: Mapped[str] = mapped_column(String(32), primary_key=True, default=new_id)
    run_id: Mapped[str] = mapped_column(ForeignKey("runs.id"), index=True)
    hypothesis: Mapped[str | None] = mapped_column(Text, nullable=True)
    metrics: Mapped[dict[str, object]] = mapped_column(JSON, default=dict)
    accepted: Mapped[bool | None] = mapped_column(nullable=True)
    summary: Mapped[str | None] = mapped_column(Text, nullable=True)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), server_default=func.now())

    run: Mapped[Run] = relationship(back_populates="experiment_results")
