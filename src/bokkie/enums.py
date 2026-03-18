from __future__ import annotations

from enum import StrEnum


class RunType(StrEnum):
    FEATURE = "feature"
    NEW_APP = "new-app"
    OPTIMISATION = "optimisation"
    EXPERIMENT = "experiment"
    RECURRING_JOB = "recurring-job"


class RiskLevel(StrEnum):
    LOW = "low"
    MEDIUM = "medium"
    HIGH = "high"


class RunStage(StrEnum):
    INTAKE = "intake"
    PLANNING = "planning"
    REVIEW_GATE_PLAN = "review_gate_plan"
    WORK_ITEM_GENERATION = "work_item_generation"
    EXECUTE = "execute"
    VERIFY = "verify"
    REVIEW_GATE_VERIFY = "review_gate_verify"
    PUBLISH = "publish"
    CONTINUE = "continue"
    STOP = "stop"


class RunStatus(StrEnum):
    QUEUED = "queued"
    RUNNING = "running"
    WAITING_REVIEW = "waiting_review"
    PAUSED = "paused"
    BLOCKED = "blocked"
    DONE = "done"
    FAILED = "failed"


class WorkItemKind(StrEnum):
    PLAN = "plan"
    IMPLEMENT = "implement"
    VERIFY = "verify"
    REVIEW = "review"
    SUMMARIZE = "summarize"
    PUBLISH = "publish"


class WorkItemStatus(StrEnum):
    QUEUED = "queued"
    LEASED = "leased"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"


class ReviewGate(StrEnum):
    PLAN = "plan"
    VERIFY = "verify"


class ReviewStatus(StrEnum):
    PENDING = "pending"
    APPROVED = "approved"
    REJECTED = "rejected"


class ArtifactKind(StrEnum):
    PATCH = "patch"
    PLAN = "plan"
    VERIFY_REPORT = "verify_report"
    LOG = "log"
    FINAL_MESSAGE = "final_message"


class WorkerState(StrEnum):
    IDLE = "idle"
    BUSY = "busy"
    OFFLINE = "offline"


class PublishStrategy(StrEnum):
    NONE = "none"
    PUSH = "push"
