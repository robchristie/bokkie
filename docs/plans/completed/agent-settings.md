# Conversational agent settings

- Status: complete
- Delivery state: acceptance-complete
- Acceptance state: passed
- Acceptance evidence: [Main and adviser qualification](../../agent-adviser-evidence/README.md)
- Landing evidence: https://github.com/robchristie/bokkie/pull/52
- Landed pull requests: https://github.com/robchristie/bokkie/pull/51
- Production deployment: separate authority; source delivery only

## Outcome

Polyorama Settings configures Bokkie's main conversational role and one optional
Astra adviser. Advertised model/thinking choices, supplementary instructions and
finite limits govern real contained execution. SQLite owns versioned editable
settings after exact deployment bootstrap; deployment retains security ceilings.
Accepted requests pin both roles, instructions and restrictions. Existing task,
schedule, review, login, credential and Docker/Bubblewrap contracts remain valid.

The main-role foundation landed in PR #51 at
`ee687ffea2875bb30afc2f4abf68e6f366b7caf5`, with an identical reviewed tree and
passing post-merge CI. The adviser uses role contract version 2: at most four
shared calls, one tool-free consultation and one empty-lookup continuation.
Manual consultation is explicit; automatic consultation requires enabled policy,
a typed conflicting-requirements report and two grounded quotes. Bokkie remains
the user-facing assistant; activity and outcomes survive reopening/restart.

## Acceptance

- [x] Existing deployment model, effort and finite limits are preserved at bootstrap.
- [x] Atomic saves survive restart; unsupported/stale combinations do not activate.
- [x] Accepted requests retain both profiles across settings edits and retries.
- [x] Durable dispatch/outcome records prevent duplicate completed or uncertain calls.
- [x] Configured adviser execution returns advice through Bokkie without new authority.
- [x] Timeout, failure, malformed advice and exhausted budgets remain understandable.
- [x] Existing task, schedule, review and confirmation guards pass canonical checks.
- [x] Desktop/narrow editing, validation, saving and conversation return are exercised.
- [x] All ten captured browser images were opened and judged.
- [x] Bounded live routing used exactly two provider turns; replay added zero calls.

## Qualification and limits

[Retained evidence](../../agent-adviser-evidence/README.md) covers 219 Python,
363 backend library and 21 conversation adapter tests, 108 UI tests, native/Wasm
builds, lint/format checks and the physical browser journey. The real stalled-peer
regression proves timeout, process reaping, Bokkie's return and free replay.

Codex 0.160.0 accepted the automatic difficulty schema without a model turn.
`gpt-6-astra`/`high` advice and `gpt-6.1-sol`/`medium` return were qualified on the
already authorised account using disposable local state and the existing
qualified container boundary. Difficulty grounding validates Bokkie's declared
condition; it does not independently infer natural-language incompatibility.
Other runtime versions still require containment qualification.

[Deployment and rollback](../../agent-settings.md#deployment-and-rollback)
require a separately authorised update and a stopped-state backup. Version 2
profiles cannot be interpreted by the main-settings-only binary despite matching
SQL schema numbers. Worker teams, scheduled research and project hand-offs remain
later work packages.
