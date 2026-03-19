from __future__ import annotations

from datetime import datetime
from typing import Any, Literal

from pydantic import BaseModel, Field

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
    type: RunType = RunType.CHANGE
    task_name: str | None = None
    objective: str
    success_criteria: str
    risk_level: RiskLevel = RiskLevel.MEDIUM
    budget: Budget = Field(default_factory=Budget)
    resource_profile: ResourceProfile = Field(default_factory=ResourceProfile)
    base_ref: str | None = None
    publish_strategy: PublishStrategy = PublishStrategy.NONE


class PhaseAttemptSummary(BaseModel):
    id: str
    phase_name: PhaseName
    phase_index: int
    attempt_no: int
    role: PhaseRole
    status: PhaseAttemptStatus
    requested_pool: str | None = None
    assigned_executor_name: str | None = None
    worker_id: str | None = None
    retry_count: int
    retry_limit: int
    dispatch_attempts: int
    thread_id: str | None = None
    last_turn_id: str | None = None
    payload: dict[str, Any]
    result: dict[str, Any] | None = None
    error_text: str | None = None
    created_at: datetime
    updated_at: datetime

    model_config = {"from_attributes": True}

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
    task_name: str | None = None
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
    run_root: str
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
    phase_attempts: list[PhaseAttemptSummary] = Field(default_factory=list)
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


class ExecutorRead(BaseModel):
    name: str
    driver: str
    host: str | None = None
    pools: list[str] = Field(default_factory=list)
    labels: list[str] = Field(default_factory=list)
    secrets: list[str] = Field(default_factory=list)
    image: str | None = None
    workdir: str | None = None
    worker_command: str | None = None
    max_workers: int = 1
    active_workers: list[WorkerRead] = Field(default_factory=list)
    pending_phase_count: int = 0


class OperatorDecision(BaseModel):
    reason: str | None = None
    actor: str = "operator"
    override: bool = False


class OperatorNoteIn(BaseModel):
    note: str
    created_by: str = "operator"


class PromoteRunIn(BaseModel):
    pool: str


class PhaseAttemptEventIn(BaseModel):
    event_type: str
    summary: str | None = None
    payload: dict[str, Any] = Field(default_factory=dict)
    worker_id: str | None = None


class PhaseAttemptCompletionIn(BaseModel):
    success: bool = True
    worker_id: str
    summary: str | None = None
    result: dict[str, Any] = Field(default_factory=dict)
    error_text: str | None = None


class PhaseLeaseResponse(BaseModel):
    leased: bool
    phase_attempt: PhaseAttemptSummary | None = None
    run: RunRead | None = None
    project: ProjectRead | None = None
    prior_patch_downloads: list[str] = Field(default_factory=list)
    input_artifacts: dict[str, str] = Field(default_factory=dict)
    operator_notes: list[str] = Field(default_factory=list)
    evaluator_commands: list[str] = Field(default_factory=list)


class TelegramReply(BaseModel):
    text: str


class PlanPhaseResult(BaseModel):
    summary: str
    next_action: str
    proposal_md: str
    design_md: str
    tasks_md: str
    blockers: list[str] = Field(default_factory=list)
    risk_flags: list[str] = Field(default_factory=list)


class ReviewPhaseResult(BaseModel):
    verdict: Literal["approve", "revise"]
    summary: str
    concerns: list[str] = Field(default_factory=list)
    next_action: str


class SpecPhaseResult(BaseModel):
    summary: str
    next_action: str
    program_md: str
    acceptance_checks: list[str] = Field(default_factory=list)


class ExecutePhaseResult(BaseModel):
    summary: str
    changed_files: list[str] = Field(default_factory=list)
    checkpoints: list[str] = Field(default_factory=list)
    next_action: str | None = None


class VerifyCommandResult(BaseModel):
    command: str
    exit_code: int
    stdout: str = ""
    stderr: str = ""


class VerifyPhaseResult(BaseModel):
    summary: str
    pass_: bool = Field(alias="pass")
    findings: list[str] = Field(default_factory=list)
    confidence: str = "medium"
    next_action: str
    command_results: list[VerifyCommandResult] = Field(default_factory=list)

    model_config = {"populate_by_name": True}
