from __future__ import annotations

from datetime import datetime
from uuid import uuid4

from sqlalchemy import JSON, DateTime, ForeignKey, Integer, String, Text, UniqueConstraint, func
from sqlalchemy.orm import Mapped, mapped_column, relationship

from .db import Base
from .enums import (
    ArtifactKind,
    PublishStrategy,
    ReviewGate,
    ReviewStatus,
    RiskLevel,
    RunStage,
    RunStatus,
    RunType,
    WorkerState,
    WorkItemKind,
    WorkItemStatus,
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


class Run(Base, TimestampMixin):
    __tablename__ = "runs"

    id: Mapped[str] = mapped_column(String(32), primary_key=True, default=new_id)
    project_id: Mapped[str] = mapped_column(ForeignKey("projects.id"), index=True)
    type: Mapped[str] = mapped_column(String(32), default=RunType.FEATURE.value)
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
    current_worker: Mapped[Worker | None] = relationship(back_populates="runs")
    work_items: Mapped[list[WorkItem]] = relationship(back_populates="run")
    reviews: Mapped[list[Review]] = relationship(back_populates="run")
    artifacts: Mapped[list[Artifact]] = relationship(back_populates="run")
    events: Mapped[list[Event]] = relationship(back_populates="run")
    notes: Mapped[list[OperatorNote]] = relationship(back_populates="run")
    experiment_results: Mapped[list[ExperimentResult]] = relationship(back_populates="run")


class WorkItem(Base, TimestampMixin):
    __tablename__ = "work_items"
    __table_args__ = (UniqueConstraint("run_id", "sequence_no", name="uq_run_sequence"),)

    id: Mapped[str] = mapped_column(String(32), primary_key=True, default=new_id)
    run_id: Mapped[str] = mapped_column(ForeignKey("runs.id"), index=True)
    sequence_no: Mapped[int] = mapped_column(Integer)
    kind: Mapped[str] = mapped_column(String(32), default=WorkItemKind.PLAN.value)
    status: Mapped[str] = mapped_column(String(32), default=WorkItemStatus.QUEUED.value, index=True)
    prompt_template: Mapped[str] = mapped_column(String(120))
    requested_pool: Mapped[str | None] = mapped_column(String(120), nullable=True)
    requires_internet: Mapped[bool] = mapped_column(default=False)
    required_secrets: Mapped[list[str]] = mapped_column(JSON, default=list)
    timeout_seconds: Mapped[int] = mapped_column(Integer, default=1800)
    retry_limit: Mapped[int] = mapped_column(Integer, default=1)
    retry_count: Mapped[int] = mapped_column(Integer, default=0)
    base_ref: Mapped[str | None] = mapped_column(String(120), nullable=True)
    branch_name: Mapped[str | None] = mapped_column(String(160), nullable=True)
    worktree_path: Mapped[str | None] = mapped_column(Text, nullable=True)
    worker_id: Mapped[str | None] = mapped_column(ForeignKey("workers.id"), nullable=True)
    lease_expires_at: Mapped[datetime | None] = mapped_column(
        DateTime(timezone=True), nullable=True
    )
    started_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    completed_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    payload: Mapped[dict[str, object]] = mapped_column(JSON, default=dict)
    result: Mapped[dict[str, object] | None] = mapped_column(JSON, nullable=True)
    error_text: Mapped[str | None] = mapped_column(Text, nullable=True)

    run: Mapped[Run] = relationship(back_populates="work_items")
    worker: Mapped[Worker | None] = relationship()
    leases: Mapped[list[Lease]] = relationship(back_populates="work_item")
    events: Mapped[list[Event]] = relationship(back_populates="work_item")
    artifacts: Mapped[list[Artifact]] = relationship(back_populates="work_item")


class Review(Base, TimestampMixin):
    __tablename__ = "reviews"

    id: Mapped[str] = mapped_column(String(32), primary_key=True, default=new_id)
    run_id: Mapped[str] = mapped_column(ForeignKey("runs.id"), index=True)
    gate: Mapped[str] = mapped_column(String(32), default=ReviewGate.PLAN.value)
    status: Mapped[str] = mapped_column(String(32), default=ReviewStatus.PENDING.value, index=True)
    summary: Mapped[str | None] = mapped_column(Text, nullable=True)
    payload: Mapped[dict[str, object]] = mapped_column(JSON, default=dict)
    decision_reason: Mapped[str | None] = mapped_column(Text, nullable=True)
    decided_by: Mapped[str | None] = mapped_column(String(120), nullable=True)
    decided_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)

    run: Mapped[Run] = relationship(back_populates="reviews")


class Artifact(Base, TimestampMixin):
    __tablename__ = "artifacts"

    id: Mapped[str] = mapped_column(String(32), primary_key=True, default=new_id)
    run_id: Mapped[str] = mapped_column(ForeignKey("runs.id"), index=True)
    work_item_id: Mapped[str | None] = mapped_column(ForeignKey("work_items.id"), nullable=True)
    kind: Mapped[str] = mapped_column(String(32), default=ArtifactKind.LOG.value)
    name: Mapped[str] = mapped_column(String(200))
    storage_path: Mapped[str] = mapped_column(Text)
    content_type: Mapped[str] = mapped_column(String(120), default="application/octet-stream")
    sha256: Mapped[str] = mapped_column(String(64))
    size_bytes: Mapped[int] = mapped_column(Integer)
    metadata_json: Mapped[dict[str, object]] = mapped_column("metadata", JSON, default=dict)

    run: Mapped[Run] = relationship(back_populates="artifacts")
    work_item: Mapped[WorkItem | None] = relationship(back_populates="artifacts")


class Event(Base):
    __tablename__ = "events"

    id: Mapped[int] = mapped_column(Integer, primary_key=True, autoincrement=True)
    run_id: Mapped[str] = mapped_column(ForeignKey("runs.id"), index=True)
    work_item_id: Mapped[str | None] = mapped_column(ForeignKey("work_items.id"), nullable=True)
    worker_id: Mapped[str | None] = mapped_column(ForeignKey("workers.id"), nullable=True)
    event_type: Mapped[str] = mapped_column(String(120), index=True)
    summary: Mapped[str | None] = mapped_column(Text, nullable=True)
    payload: Mapped[dict[str, object]] = mapped_column(JSON, default=dict)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), server_default=func.now())

    run: Mapped[Run] = relationship(back_populates="events")
    work_item: Mapped[WorkItem | None] = relationship(back_populates="events")


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
    work_item_id: Mapped[str] = mapped_column(ForeignKey("work_items.id"), index=True)
    worker_id: Mapped[str] = mapped_column(ForeignKey("workers.id"), index=True)
    acquired_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), server_default=func.now()
    )
    expires_at: Mapped[datetime] = mapped_column(DateTime(timezone=True))
    released_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    release_reason: Mapped[str | None] = mapped_column(Text, nullable=True)

    work_item: Mapped[WorkItem] = relationship(back_populates="leases")


class OperatorNote(Base):
    __tablename__ = "operator_notes"

    id: Mapped[str] = mapped_column(String(32), primary_key=True, default=new_id)
    run_id: Mapped[str] = mapped_column(ForeignKey("runs.id"), index=True)
    note: Mapped[str] = mapped_column(Text)
    created_by: Mapped[str] = mapped_column(String(120), default="operator")
    applied_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), server_default=func.now())

    run: Mapped[Run] = relationship(back_populates="notes")


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
