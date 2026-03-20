# Campaign-First Milestone

## Goal

Reshape Bokkie from a run-form-first control plane into a campaign-first, chat-first system for long-running autonomous research/development loops, while preserving:

- the explicit phase-based run engine
- worker/executor orchestration
- artifact-first handoff between phases
- operator approvals and steering
- Codex app-server integration
- DB as orchestration/index source of truth

This milestone is intentionally a small vertical slice, not a rewrite.

## Baseline Observations

- The current primary UI is [`src/bokkie/templates/runs.html`](/nvme/development/bokkie/src/bokkie/templates/runs.html) and is centered on a manual "Create Run" form.
- The current control plane is centered on `Run`; there is no campaign object above it yet.
- The orchestrator in [`src/bokkie/services/orchestrator.py`](/nvme/development/bokkie/src/bokkie/services/orchestrator.py) implements the `change` flow end to end.
- `bokkie.toml` already declares `investigation` and `experiment` run types, but the orchestrator only supports the `change`-style phases today.
- `tasks/autoresearch.toml` still declares `run_type = "change"` and needs to be reconciled with research/experiment intent.
- Telegram currently supports run-oriented commands only.

## Milestone Checklist

- [x] Add campaign-first DB model(s) and schemas.
- [x] Link campaigns to runs and add campaign artifact roots under `.bokkie/campaigns/<campaign-id>/...`.
- [x] Add a setup agent prompt and structured draft schema.
- [x] Add draft-first intake APIs for natural-language campaign setup.
- [x] Add draft approval flow that creates a campaign plus its first run.
- [x] Rework primary UI around intake + campaigns while keeping manual run creation available.
- [x] Extend orchestrator with campaign-aware summaries, notes, and steering.
- [x] Implement one experiment/research iteration vertical slice using the existing run engine.
- [x] Add lightweight budget/policy guardrails, including approval boundaries.
- [x] Add Telegram `/new` plus campaign summary/steering support.
- [x] Update README/docs with campaign-first concepts and example usage.
- [x] Add tests for draft creation, approval, campaign/run linkage, pages, steering, Telegram, and compatibility.
- [x] Run repo validation commands.

## Current Status

- Campaign, campaign draft, and campaign-note aware models and schemas are implemented.
- The root web UX now redirects to `/ui/intake`, which supports free-text draft generation and draft approval into campaign + first run.
- Campaign list/detail pages exist and the old run form remains available as the advanced/manual path.
- `experiment` / research-style runs now execute `plan -> execute -> verify -> analyze -> propose_next`.
- Auto-continuation works for in-policy experiment proposals; otherwise Bokkie pauses at an approval gate.
- Campaign setup artifacts and notebook files are written under `.bokkie/campaigns/<campaign-id>/...`.
- Telegram now supports `/new`, `/campaigns`, `/campaign`, and `/steer_campaign`, while `/approve` and `/reject` can resolve drafts/campaign gates/runs.
- Validation completed with:
  - `uv run ruff format --check src tests`
  - `uv run ruff check src tests`
  - `uv run pytest -q`

## Implementation Plan

1. Add minimal campaign data model.
   - Introduce `Campaign` as a long-lived parent of `Run`.
   - Add campaign-level steering notes and setup drafts using JSON fields where this reduces schema churn.
   - Add `campaign_id` to `Run`.

2. Add draft-first intake.
   - Create a read-only `setup` agent prompt.
   - Add a structured `CampaignDraft` schema returned from free text.
   - Implement a draft-generation service path that infers project, campaign type, first run type, task preset, pool, internet, budget, continuation policy, approval gates, and rationale.
   - Make approval create both the `Campaign` and the first bounded `Run`.

3. Add campaign-aware orchestration.
   - Keep the existing run engine as the execution primitive.
   - Extend the orchestrator so campaign summaries, notes, notebook updates, and next-action state are maintained at the campaign level.
   - Support queued steering for the next safe boundary if no active run is present.

4. Add one concrete research iteration slice.
   - Implement an `experiment` run-type phase flow that supports:
     - plan
     - execute
     - verify
     - analyze
     - propose_next
   - Keep the implementation generic in core control-plane code.
   - Use a repo task preset to express the research iteration behavior.

5. Add minimal operator surfaces.
   - New intake page with free-text prompt as the primary entrypoint.
   - Campaign list/detail pages.
   - Manual run creation remains available as an advanced/manual path.
   - Telegram support for draft creation, campaign status, approval, and steering.

6. Close with docs/tests/validation.

## Decisions Made

- Reuse the existing `Run` execution model and phase-attempt machinery rather than creating a second orchestration subsystem.
- Use `Campaign` as the parent object and keep `Run` as the bounded execution unit.
- Use JSON fields for first-slice budget, continuation policy, approval gates, and draft payloads to keep schema churn small.
- Keep the DB as the orchestration index/source of truth, while files under `.bokkie/campaigns/...` and `.bokkie/runs/...` remain the canonical handoff and audit surfaces.
- Keep one active child run per campaign for this milestone.
- Keep the old manual run path for backward compatibility, but demote it in the UI.
- Keep draft generation deterministic and heuristic-based for this milestone, while still adding a real `agents/setup/PROMPT.md` contract for a future live setup-agent execution path.
- Reconcile the configured research flow by making `experiment` and `investigation` use supported phase names and by moving `tasks/autoresearch.toml` onto `run_type = "experiment"`.
- Make `uv run pytest -q` work as documented by moving test/lint tools into a default `uv` dependency group.

## Follow-Ups / Known Gaps

- Draft generation is currently heuristic and deterministic; it does not yet execute a live read-only Codex setup turn.
- Auto-continuation is intentionally limited to experiment/research-style campaigns in this slice.
- Budget enforcement is lightweight: max iterations, max active runs, and coarse total-cost checks exist, but there is no deep spend accounting yet.
- Research-branch/data-family checks still depend on structured `propose_next` outputs rather than semantic repo-aware branch classification.
- Campaign steering is stored and delivered to active or next relevant iterations, but there is not yet a richer per-phase steering inbox UI beyond the existing run/campaign note surfaces.
