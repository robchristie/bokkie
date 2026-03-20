from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from .config import Settings
from .enums import PhaseName, PhaseRole
from .models import PhaseAttempt, Project, Run


def _render_notes(operator_notes: list[str]) -> str:
    if not operator_notes:
        return "No pending operator notes."
    return "\n".join(f"- {note}" for note in operator_notes)


def load_agent_prompt(settings: Settings, role: str) -> str:
    path = settings.resolved_repo_root() / "agents" / role / "PROMPT.md"
    if not path.exists():
        return ""
    return path.read_text().strip()


def _read_artifacts(worktree: Path, input_artifacts: dict[str, str]) -> str:
    rendered: list[str] = []
    for relative_path in sorted(input_artifacts):
        artifact_path = worktree / relative_path
        if not artifact_path.exists():
            continue
        rendered.append(f"## {relative_path}\n{artifact_path.read_text()}")
    return "\n\n".join(rendered) if rendered else "No prior artifacts materialized in the worktree."


def build_phase_prompt(
    settings: Settings,
    project: Project,
    run: Run,
    phase_attempt: PhaseAttempt,
    worktree: Path,
    operator_notes: list[str],
    input_artifacts: dict[str, str],
    evaluator_commands: list[str],
) -> str:
    role = phase_attempt.role
    phase_name = phase_attempt.phase_name
    base_prompt = load_agent_prompt(settings, role)
    artifacts_text = _read_artifacts(worktree, input_artifacts)
    evaluator_text = "\n".join(f"- {command}" for command in evaluator_commands) or "- None"
    payload_text = json.dumps(phase_attempt.payload, indent=2, sort_keys=True)
    return f"""
{base_prompt}

Project:
- Name: {project.name}
- Repository: {project.repo_url}
- Default branch: {project.default_branch}

Run:
- Type: {run.type}
- Campaign: {run.campaign_id or "none"}
- Iteration: {run.iteration_no or "n/a"}
- Task preset: {run.task_name or "none"}
- Objective: {run.objective}
- Success criteria: {run.success_criteria}
- Risk level: {run.risk_level}
- Branch: {run.branch_name}
- Base ref: {run.base_ref}

Phase:
- Name: {phase_name}
- Role: {role}
- Attempt: {phase_attempt.attempt_no}
- Payload:
{payload_text}

Operator notes:
{_render_notes(operator_notes)}

Materialized artifacts:
{artifacts_text}

Evaluator commands:
{evaluator_text}

Instructions:
- Work only on the current phase objective.
- Treat the materialized artifact bundle as the contract with other phases.
- Preserve any existing changes already present in the worktree.
- Return only content that matches the provided JSON schema.
""".strip()


def phase_role_for(phase_name: str) -> str:
    if phase_name in {
        PhaseName.PLAN.value,
        PhaseName.SPEC.value,
        PhaseName.ANALYZE.value,
        PhaseName.PROPOSE_NEXT.value,
    }:
        return PhaseRole.PLANNER.value
    if phase_name in {
        PhaseName.PLAN_REVIEW.value,
        PhaseName.SPEC_REVIEW.value,
        PhaseName.FINAL_REVIEW.value,
    }:
        return PhaseRole.REVIEWER.value
    if phase_name == PhaseName.VERIFY.value:
        return PhaseRole.VERIFIER.value
    return PhaseRole.CODER.value


def summarize_event_line(event: dict[str, Any]) -> str | None:
    event_type = event.get("method") or event.get("type")
    if not event_type:
        return None
    summaries = {
        "thread/started": "Codex thread started",
        "turn/started": "Codex turn started",
        "turn/completed": "Codex turn completed",
        "thread/status/changed": "Thread status changed",
        "item/started": "Codex item started",
        "item/completed": "Codex item completed",
        "item/agentMessage/delta": "Assistant message delta",
        "item/commandExecution/outputDelta": "Command output delta",
        "error": "Codex app-server error",
    }
    return summaries.get(event_type, event_type)
