from __future__ import annotations

from typing import Any

from .models import Project, Run, WorkItem


def _render_notes(operator_notes: list[str]) -> str:
    if not operator_notes:
        return "No pending operator notes."
    return "\n".join(f"- {note}" for note in operator_notes)


def planner_prompt(
    project: Project,
    run: Run,
    work_item: WorkItem,
    operator_notes: list[str],
) -> str:
    return f"""
You are the planning worker for a bounded software development run.

Project:
- Name: {project.name}
- Repository: {project.repo_url}
- Default branch: {project.default_branch}

Run:
- Type: {run.type}
- Objective: {run.objective}
- Success criteria: {run.success_criteria}
- Risk level: {run.risk_level}
- Preferred pool: {run.preferred_pool or "unspecified"}
- Requires internet: {run.requires_internet}
- Required secrets: {", ".join(run.required_secrets) or "none"}

Operator notes:
{_render_notes(operator_notes)}

Budget:
{run.budget}

Instructions:
- Inspect the repository and produce a concrete implementation plan.
- Break the plan into bounded implementation work items.
- Keep the work items sequential and execution-safe.
- Call out blockers and risks clearly.
- Do not make code changes if read-only constraints apply.
- Return only content that matches the provided JSON schema.
""".strip()


def implement_prompt(
    project: Project,
    run: Run,
    work_item: WorkItem,
    operator_notes: list[str],
) -> str:
    instructions = work_item.payload.get("instructions", "")
    title = work_item.payload.get("title", work_item.prompt_template)
    return f"""
You are the executor for a bounded software development work item.

Project: {project.name}
Run objective: {run.objective}
Success criteria: {run.success_criteria}
Work item title: {title}
Work item instructions:
{instructions}

Operator notes:
{_render_notes(operator_notes)}

Constraints:
- Work only within this work item.
- Preserve previous changes already present in the worktree.
- Make pragmatic progress toward the run objective.
- If tests or checks are available, run the most relevant ones.
- Return only content that matches the provided JSON schema.
""".strip()


def verify_prompt(
    project: Project,
    run: Run,
    work_item: WorkItem,
    operator_notes: list[str],
) -> str:
    verify_commands = project.command_profiles.get("verify", [])
    verify_text = (
        verify_commands if verify_commands else ["Inspect the diff and run focused verification."]
    )
    return f"""
You are the verifier for a bounded software development run.

Project: {project.name}
Run objective: {run.objective}
Success criteria: {run.success_criteria}

Operator notes:
{_render_notes(operator_notes)}

Verify using these commands or checks when possible:
{verify_text}

Instructions:
- Review the current worktree as if you were an exacting reviewer.
- Focus on correctness, regressions, and missing validation.
- Summarize whether the run is ready for publish.
- Return only content that matches the provided JSON schema.
""".strip()


def repair_prompt(
    project: Project,
    run: Run,
    work_item: WorkItem,
    operator_notes: list[str],
) -> str:
    verifier_findings = work_item.payload.get("verifier_findings", [])
    reason = work_item.payload.get("rejection_reason", "")
    return f"""
You are the executor for a rework item created after verification feedback.

Project: {project.name}
Run objective: {run.objective}
Success criteria: {run.success_criteria}

Verifier findings:
{verifier_findings}

Operator rejection reason:
{reason}

Operator notes:
{_render_notes(operator_notes)}

Instructions:
- Address the verifier and operator feedback directly.
- Keep the scope bounded to the reported issues.
- Return only content that matches the provided JSON schema.
""".strip()


def build_prompt(
    project: Project,
    run: Run,
    work_item: WorkItem,
    operator_notes: list[str],
) -> str:
    if work_item.kind == "plan":
        return planner_prompt(project, run, work_item, operator_notes)
    if work_item.kind == "verify":
        return verify_prompt(project, run, work_item, operator_notes)
    if work_item.payload.get("verifier_findings"):
        return repair_prompt(project, run, work_item, operator_notes)
    return implement_prompt(project, run, work_item, operator_notes)


def summarize_event_line(event: dict[str, Any]) -> str | None:
    event_type = event.get("type")
    if not event_type:
        return None
    if event_type == "thread.started":
        return "Codex thread started"
    if event_type == "turn.started":
        return "Codex turn started"
    if event_type == "turn.completed":
        return "Codex turn completed"
    if event_type == "turn.failed":
        return "Codex turn failed"
    if event_type.startswith("item."):
        return event_type
    return None
