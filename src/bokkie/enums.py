from __future__ import annotations

from enum import StrEnum


class RunType(StrEnum):
    CHANGE = "change"
    INVESTIGATION = "investigation"
    EXPERIMENT = "experiment"
    RECURRING_JOB = "recurring-job"


class RiskLevel(StrEnum):
    LOW = "low"
    MEDIUM = "medium"
    HIGH = "high"


class RunStage(StrEnum):
    INTAKE = "intake"
    PLAN = "plan"
    PLAN_REVIEW = "plan_review"
    SPEC = "spec"
    SPEC_REVIEW = "spec_review"
    EXECUTE = "execute"
    VERIFY = "verify"
    FINAL_REVIEW = "final_review"
    PUBLISH = "publish"
    DONE = "done"


class RunStatus(StrEnum):
    QUEUED = "queued"
    RUNNING = "running"
    WAITING_REVIEW = "waiting_review"
    PAUSED = "paused"
    BLOCKED = "blocked"
    DONE = "done"
    FAILED = "failed"


class PhaseAttemptStatus(StrEnum):
    QUEUED = "queued"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"


class PhaseName(StrEnum):
    PLAN = "plan"
    PLAN_REVIEW = "plan_review"
    SPEC = "spec"
    SPEC_REVIEW = "spec_review"
    EXECUTE = "execute"
    VERIFY = "verify"
    FINAL_REVIEW = "final_review"


class PhaseRole(StrEnum):
    PLANNER = "planner"
    REVIEWER = "reviewer"
    CODER = "coder"
    VERIFIER = "verifier"


class ReviewGate(StrEnum):
    PLAN = "plan_review"
    SPEC = "spec_review"
    FINAL = "final_review"


class ReviewStatus(StrEnum):
    PENDING = "pending"
    APPROVED = "approved"
    REJECTED = "rejected"


class ArtifactKind(StrEnum):
    REQUEST = "request"
    STATUS = "status"
    PROPOSAL = "proposal"
    DESIGN = "design"
    TASKS = "tasks"
    PLAN_JSON = "plan_json"
    REVIEW_JSON = "review_json"
    PROGRAM = "program"
    SPEC_JSON = "spec_json"
    PATCH = "patch"
    CHECKPOINT = "checkpoint"
    LOG = "log"
    VERIFY_RESULTS = "verify_results"
    VERIFY_REVIEW = "verify_review"
    FINAL_MESSAGE = "final_message"


class WorkerState(StrEnum):
    IDLE = "idle"
    BUSY = "busy"
    OFFLINE = "offline"


class PublishStrategy(StrEnum):
    NONE = "none"
    PUSH = "push"
