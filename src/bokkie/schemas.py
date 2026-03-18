from __future__ import annotations

from datetime import datetime
from typing import Any

from pydantic import BaseModel, Field

from .enums import (
    ArtifactKind,
    PublishStrategy,
    ReviewGate,
    ReviewStatus,
    RiskLevel,
    RunStage,
    RunStatus,
    RunType,
    WorkItemKind,
    WorkItemStatus,
)


class Budget(BaseModel):
    max_turns: int | None = None
    max_wall_clock: int | None = None
    max_cost: float | None = None


class ResourceProfile(BaseModel):
    pool: str | None = None
    internet: bool = False
    secrets: list[str] = Field(default_factory=list)


class ProjectCreate(BaseModel):
    slug: str
    name: str
    repo_url: str
    default_branch: str = "main"
    push_remote: str | None = None
    allowed_pools: list[str] = Field(default_factory=list)
    required_secrets: list[str] = Field(default_factory=list)
    command_profiles: dict[str, Any] = Field(default_factory=dict)
    settings: dict[str, Any] = Field(default_factory=dict)


class ProjectRead(ProjectCreate):
    id: str
    created_at: datetime
    updated_at: datetime

    model_config = {"from_attributes": True}


class RunCreate(BaseModel):
    project_id: str
    type: RunType = RunType.FEATURE
    objective: str
    success_criteria: str
    risk_level: RiskLevel = RiskLevel.MEDIUM
    budget: Budget = Field(default_factory=Budget)
    resource_profile: ResourceProfile = Field(default_factory=ResourceProfile)
    base_ref: str | None = None
    publish_strategy: PublishStrategy = PublishStrategy.NONE


class WorkItemSummary(BaseModel):
    id: str
    sequence_no: int
    kind: WorkItemKind
    status: WorkItemStatus
    prompt_template: str
    requested_pool: str | None = None
    worker_id: str | None = None
    payload: dict[str, Any]
    result: dict[str, Any] | None
    error_text: str | None
    created_at: datetime
    updated_at: datetime

    model_config = {"from_attributes": True}


class ReviewSummary(BaseModel):
    id: str
    gate: ReviewGate
    status: ReviewStatus
    summary: str | None = None
    decision_reason: str | None = None
    decided_by: str | None = None
    decided_at: datetime | None = None
    created_at: datetime
    updated_at: datetime

    model_config = {"from_attributes": True}


class ArtifactSummary(BaseModel):
    id: str
    kind: ArtifactKind
    name: str
    content_type: str
    size_bytes: int
    metadata_json: dict[str, Any]
    created_at: datetime
    download_url: str | None = None

    model_config = {"from_attributes": True}


class EventRead(BaseModel):
    id: int
    event_type: str
    summary: str | None = None
    payload: dict[str, Any]
    created_at: datetime

    model_config = {"from_attributes": True}


class RunRead(BaseModel):
    id: str
    project_id: str
    type: RunType
    objective: str
    success_criteria: str
    risk_level: RiskLevel
    budget: dict[str, Any]
    resource_profile: dict[str, Any]
    current_stage: RunStage
    current_session_id: str | None = None
    status: RunStatus
    base_ref: str | None = None
    branch_name: str
    latest_summary: str | None = None
    current_worker_id: str | None = None
    latest_verifier_result: dict[str, Any] | None = None
    next_action: str | None = None
    blockers: list[str]
    risk_flags: list[str]
    preferred_pool: str | None = None
    requires_internet: bool
    required_secrets: list[str]
    publish_strategy: PublishStrategy
    created_at: datetime
    updated_at: datetime
    started_at: datetime | None = None
    completed_at: datetime | None = None
    work_items: list[WorkItemSummary] = Field(default_factory=list)
    reviews: list[ReviewSummary] = Field(default_factory=list)
    artifacts: list[ArtifactSummary] = Field(default_factory=list)
    events: list[EventRead] = Field(default_factory=list)

    model_config = {"from_attributes": True}


class WorkerCapabilities(BaseModel):
    id: str
    host: str
    pools: list[str] = Field(default_factory=list)
    labels: list[str] = Field(default_factory=list)
    secrets: list[str] = Field(default_factory=list)
    cpu_cores: int | None = None
    ram_gb: int | None = None
    gpu_model: str | None = None
    gpu_vram_gb: int | None = None
    metadata: dict[str, Any] = Field(default_factory=dict)


class WorkerHeartbeatIn(BaseModel):
    observed_load: int = 0
    payload: dict[str, Any] = Field(default_factory=dict)


class WorkerRead(BaseModel):
    id: str
    host: str
    pools: list[str]
    labels: list[str]
    secrets: list[str]
    cpu_cores: int | None
    ram_gb: int | None
    gpu_model: str | None
    gpu_vram_gb: int | None
    state: str
    current_load: int
    metadata_json: dict[str, Any]
    last_seen_at: datetime
    created_at: datetime
    updated_at: datetime

    model_config = {"from_attributes": True}


class OperatorDecision(BaseModel):
    reason: str | None = None
    actor: str = "operator"


class OperatorNoteIn(BaseModel):
    note: str
    created_by: str = "operator"


class PromoteRunIn(BaseModel):
    pool: str


class WorkItemEventIn(BaseModel):
    event_type: str
    summary: str | None = None
    payload: dict[str, Any] = Field(default_factory=dict)
    worker_id: str | None = None


class WorkItemCompletionIn(BaseModel):
    success: bool = True
    worker_id: str
    summary: str | None = None
    result: dict[str, Any] = Field(default_factory=dict)
    error_text: str | None = None
    patch_artifact_id: str | None = None


class WorkItemLeaseResponse(BaseModel):
    leased: bool
    work_item: WorkItemSummary | None = None
    run: RunRead | None = None
    project: ProjectRead | None = None
    prior_patch_downloads: list[str] = Field(default_factory=list)


class TelegramReply(BaseModel):
    text: str


class PlanWorkItemSpec(BaseModel):
    title: str
    instructions: str
    requested_pool: str | None = None


class PlanResult(BaseModel):
    summary: str
    next_action: str
    blockers: list[str] = Field(default_factory=list)
    risk_flags: list[str] = Field(default_factory=list)
    work_items: list[PlanWorkItemSpec] = Field(default_factory=list)


class ImplementResult(BaseModel):
    summary: str
    changed_files: list[str] = Field(default_factory=list)
    next_action: str | None = None


class VerifyResult(BaseModel):
    summary: str
    pass_: bool = Field(alias="pass")
    findings: list[str] = Field(default_factory=list)
    confidence: str = "medium"
    next_action: str

    model_config = {"populate_by_name": True}


class LeaseExtendIn(BaseModel):
    worker_id: str
