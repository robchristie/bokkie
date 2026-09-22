# Conversational task qualification

The owning [active plan](../plans/active/conversational-task-management.md)
tracks integrated acceptance. This record is provisional until the real model/UI
journey is recorded. Tests use synthetic text and temporary databases; no live
operator database, notifications, research/email access or deployment is involved.

The no-model preflight has passed against Codex 0.155.1. Empty execution
environments, ephemeral thread, disabled integrations and instruction isolation
were observed before any model invocation. Desktop 1440×900 and narrow 480×720
empty-state captures were opened: conversation, catalogue, disabled send and the
unavailable-runtime explanation were readable. A cramped navigation label was
repaired before live qualification. Two initial harness failures (header geometry
and JSON BigInt encoding) were corrected without making model calls.

Canonical backend and UI checks pass at the implementation checkpoint. The UI
check includes 75 tests, native/Wasm builds, Clippy and formatting; focused backend
checks include deterministic lifecycle, migration, conversation and real scheduler /
HTTP integration tests. Exact source and artefact identities will accompany the
accepted journey record; these statements alone do not establish final acceptance.

Live budget: one representative journey, at most 12 model calls and 15 minutes;
at most one focused repair rerun, at most 4 calls and 5 minutes. Runtime profile
settings use the existing authorised local account. No model comparison campaign
or account configuration change is authorised by this fixture.
