# Engineering supervision contract

Status: backend contract implemented; integrated runtime qualification passed.
This document defines Store semantics. Fixture and supervised application
acceptance are retained by the completed plan below; live calibration selected
the broker/containment profile. The programme and acceptance owner is
[`plans/completed/engineering-supervision.md`](plans/completed/engineering-supervision.md).
Protocol evidence belongs in `docs/supervision-evidence/`.

## Ownership and durable records

The existing `ObligationState`, attempts, leases and audit events remain the only
lifecycle. Add an engineering binding for a root supervisor obligation and each
worker package obligation. Do not reuse the gardener registration, approvals or
execution lane. A package's submission is evidence awaiting assessment, not
successful completion of its obligation.

The following typed records are retained in immutable, bounded aggregate snapshot
revisions. Relational tables bind outcomes, obligations, immutable dispatches and
workspace reservations; receipts retain the full typed envelope and trusted actor.
Foreign keys and append-only guards protect these rows. Mutable pointers select
the current revision; obligation state remains in the existing lifecycle tables.

| Record | Required identity and content |
|---|---|
| Outcome | ID, root obligation ID, current contract revision, creation command receipt |
| Contract revision | Outcome ID, increasing revision, predecessor, original intent, acceptance criteria with stable IDs, permitted scope, prohibited effects, authority grants and their decision evidence, supervisor instruction revision, finite budget, author and timestamp |
| Instruction revision | Role (`Supervisor` or `Worker`), immutable instruction text, digest, context artefact references, applicable guidance/skill identities; runtime configuration is a referenced adapter profile |
| Package | ID, outcome ID, worker obligation ID, contract revision, optional parent package ID, bounded instructions, immutable input revision references, criteria, workspace policy, budget |
| Dependency | Dependent package ID and prerequisite package ID; prerequisite must have acceptance under the current contract |
| Execution | ID, obligation occurrence and attempt, lease generation, contract revision, role, package if worker, durable dispatch key, selected instruction/context/profile identities, workspace identity/reservation, consumed budget |
| Execution checkpoint | Execution ID, increasing sequence, external runtime identity when known, external request/turn identity, protocol cursor, bounded progress summary, evidence reference and timestamp |
| Question | ID, execution ID, contract revision, stable runtime request identity (broker process generation, thread, turn, item and request ID), kind (`Routine`, `MissingInformation` or `NewAuthority`), bounded prompt/options and precise requested decision |
| Resolution | Question ID, exact contract revision, actor role, answer, supporting existing authority/criterion references; new authority requires an operator decision and a new contract revision |
| Submission | ID, execution/package/outcome IDs, exact contract revision, immutable artefact revisions, per-criterion evidence, claimed limitations and cessation evidence reference |
| Assessment | ID, exact submission ID and digest, contract revision, assessor execution ID, criterion verdicts, independent review evidence identity, decision (`Accept` or `Repair`) |
| Repair | ID, rejected submission/assessment IDs, replacement package ID, precise unmet criteria and retained useful input revisions |
| Writer reservation | Canonical workspace identity, execution ID, ownership epoch; held until verified cessation, including across lease expiry, cancellation and daemon restart |
| Reconciliation observation | Execution ID, adapter identity, observed runtime state, evidence, optional cessation proof, observation time and next action |
| Command receipt | Stable command ID, canonical typed payload digest, authority/precondition digest, immutable bounded response, resulting event sequence |

IDs are bounded strings; generated durable record IDs use UUIDs. A digest is a
validated SHA-256 identity, never an unchecked label. A Git artefact names repository identity, exact commit and
tree; a file artefact names an approved store identity, relative path, byte
length and digest. A mutable worktree path, branch name or runtime thread ID
alone cannot identify a deliverable. Validation evidence includes the exact
artefact revision, command/profile identity, exit result and retained output
digest. Evidence acquisition happens outside SQLite transactions.

Containment and prerequisites have different meanings. Parent links form a tree
within an outcome; dependency links form a separate directed acyclic graph.
Reject missing, cross-outcome, self and cyclic links atomically. Dependency dispatch requires accepted results, never provisional checkpoints.
Edges are immutable creation-time references to existing same-outcome packages;
there is no mutable dependency operation that can introduce a cycle. Cancel an
outcome or parent by requesting cancellation of its owned descendants; do not
cancel an unrelated prerequisite or dependent through a dependency edge.
Failed/cancelled prerequisites block dispatch and wake the supervisor to repair,
replan or surface attention. Acceptance of a parent requires all its required
children to be accepted or explicitly superseded by a contract-bound repair.

A supervisor may request cancellation of a named delegated package under the
current contract. Only the operator may cancel the whole outcome. Cancellation
retains fencing and writer responsibility until verified cessation. Final
acceptance may omit explicitly cancelled packages, but their assessments cannot
cover outcome criteria or satisfy surviving parent/dependency requirements.
Every current outcome criterion still requires evidence from another current,
accepted package and exact independent review. Cancelling an obsolete auxiliary
review does not waive inspection of the surviving source artefacts.

## Adapter-ready Store surface

Use one typed command boundary so HTTP, CLI and runtime adapters share replay,
revision and authority checks. The executable definitions are
[`src/engineering.rs`](../src/engineering.rs) and the
[Store extension](../src/store/engineering.rs). `engineering_command` takes its
trusted actor separately from the serialised envelope; model/HTTP JSON cannot
select that actor. `claim_due_engineering` claims role-specific work;
`engineering_outcome`, `engineering_outcome_ids` and `engineering_history_page`
provide bounded read access.

The closed command set supports outcome creation and follow-up, operator contract
revision, supervisor criterion formalisation, packages, checkpoints, questions
and resolutions, submissions, assessment, linked repair, supervisor yield,
cancellation, trusted reconciliation and final acceptance. It accepts no SQL,
arbitrary lifecycle state or general-purpose execution command.

`EngineeringPrecondition` is absent only for outcome creation; otherwise it
contains the outcome ID, expected contract revision and outcome state watermark.
The watermark includes incoming evidence and operator messages, so new input
fences decisions made from an older snapshot. Runtime mutations additionally require execution ID
and the current claim token/generation. The execution identity and all referenced
record ownership must agree. A claim carries the existing `Claim`, execution ID,
exact contract/instruction/context references, remaining budget and reserved
workspace identity. Claims atomically create dispatch intent and reserve budget
and writer ownership before returning; dispatch happens afterwards.

A successful supervisor yield acknowledges its exact current decision inputs,
including submissions awaiting delegated review; it does not assess or accept
them. While healthy workers remain active, unchanged acknowledged inputs may
retain a bounded durable wake-up without consuming another supervisor turn.
Unresolved current questions always require a supervisor decision and prevent
this coalescing. New evidence or messages invalidate the wait, and no active
worker means the supervisor must resume the decision itself.

`EngineeringActor` separates operator commands, supervisor claims, worker claims
and adapter reconciliation observations. Adapters construct actors from their
trusted call path; an HTTP payload cannot self-select supervisor, worker or
reconciler authority. The existing local operator actor remains audit evidence,
not multi-user authentication. Workers may checkpoint, ask questions and submit
their own results. Supervisors may create bounded packages, answer routine
questions, assess submissions and commission repairs within the contract.
Only an operator may widen authority. Only the configured adapter may attest
process/runtime observations; a worker's claim that it stopped is not cessation
evidence. A worker cannot assess its own submission.

`AssessmentInput` names an exact submission and criteria. `Repair` assessment
records the deficiencies and durably schedules the supervisor; `CreateRepair`
consumes that assessment exactly once and atomically creates the replacement
package and relationship. `FinishOutcome` requires current-contract accepted
results covering every required criterion, independent review evidence for the
exact final artefacts, no unresolved blocking question, no outstanding required
child and no unaccounted writer. It records a separate supervisor acceptance.
A successful worker process or a review verdict cannot substitute for it.

Every mutation validates bounded typed input, then opens one immediate
transaction. Look up the receipt before testing current state: identical replay
returns the original response even after later transitions. Reusing the ID with
a different payload, actor or precondition returns `StoreError::Conflict`.
Failed validation/transitions have no success receipt. Within that transaction,
validate authority and fences, append records, call domain transitions, append
the obligation audit event, save the receipt and commit. A lost response cannot
duplicate a package, repair, answer, submission or acceptance. The receipt
contains record IDs, revisions and event sequence; it never embeds a transcript.

## Lifecycle mappings and recovery

| Event | Authoritative obligation effect and responsibility |
|---|---|
| Intent saved | Root `Pending`, wake now; supervisor owns next action |
| Package ready | Worker `Pending`, wake now; only worker lane can claim |
| Dependency unresolved | Worker remains `Pending` with a bounded recheck wake; claim predicate excludes it; root retains its supervisor wake |
| Supervisor/worker claimed | Corresponding obligation `Running` with lease; immutable execution and dispatch intent already exist |
| Routine question | Persist request and wake root; worker lease/checkpoint remains owned while runtime waits; answer delivery has a stable request/receipt identity |
| New authority question | Root visibly `Attention` with the exact requested decision; affected execution is paused/interrupted and reconciled before any replacement |
| Worker result with proven cessation | Retire worker attempt, retain worker `Pending` with an acceptance recheck wake; root wake becomes due unless it already has an active supervisor lease |
| Supervisor continuation | Retire supervisor attempt and schedule root `Pending` at the bounded next action time; never generically mark root completed |
| Accepted package | Worker `Completed` only through assessment; wake dependent/root obligations atomically |
| Repair requested | Existing package remains nonterminal pending supersession; replacement becomes dispatchable only when writer and dependency gates pass |
| Lease expiry/runtime ambiguity | Retire expired attempt; obligation `Attention` with `NeedsReconciliation`; retain writer reservation and wake/retain supervisor responsibility |
| Cancellation requested | Fence result/dispatch authority immediately; retain visible cancellation/reconciliation responsibility while a writer may exist |
| Cessation verified after cancellation | Release reservation and terminally cancel affected obligations; root cannot disappear while descendants need reconciliation |
| Final acceptance | Root `Completed` through `FinishOutcome` only |

Worker claim selection requires a ready package with satisfied prerequisites,
remaining budget, current contract and no submission awaiting assessment or
unresolved writer ownership. Supervisor claim selection requires a due root and
remaining turn budget. `ResumeExecution` refers to the same external execution,
not a replacement writer; it requires reconciliation evidence and a fresh kernel
claim if the earlier lease expired. It consumes the recovery budget and cannot
revive cancelled or superseded authority.

Specialised Store transitions retire attempts and release kernel leases without
calling generic successful completion for an unaccepted package. Scheduler
bookkeeping wakes for dependencies/acceptance are not Codex turns and do not
consume an execution budget. `YieldSupervisor` provides a finite wake time and
durable reason, and preserves pending questions/results received during its
turn. Arriving evidence cannot overwrite an active supervisor lease or be lost
when that turn ends. Attention states always include an actionable cause and
the party responsible; an unanswered routine question alone is not a request
for the human to intervene.

Ordinary fake claims and gardener claims exclude every engineering binding.
Generic `complete`, approval, retry and cancellation routes reject engineering
bindings and direct clients to the specialised operation. Lease renewal and
expiry remain central, but use the engineering ownership checks. The shared
operator capability projection must disable unsupported generic actions. Guard
the Store boundary as well as the UI: a crafted HTTP/CLI call cannot bypass it.

A lease proves authority to update Bokkie, not exclusive control of a process.
Expiry, an interrupt request, a closed socket, a PID alone and a reported turn
completion do not prove that all external writers stopped. Writer reservations
survive all of them. Adapter cessation evidence must identify the owned runtime
or process containment boundary and establish that no descendant retains write
access. Live calibration selects the supported proof; absent such proof, retain
reconciliation attention and prohibit replacement in that workspace. An explicit
isolation policy may reserve a new, disjoint workspace with separate writable
state; it does not release the old reservation or its reconciliation duty.

Reconnection first observes the durable dispatch/runtime identities. If dispatch
acknowledgement was lost, recover that execution rather than dispatching another.
If the protocol cannot discover whether dispatch happened, remain uncertain.
Expired worker claims cannot append authoritative results. A currently fenced
reconciler may retain an offline artefact observation and, after cessation and
ownership validation, import it as a recovered submission attributed to its
original execution. Import must be explicit in `ReconciliationInput`; it must
match the current contract and unsuperseded execution and remain separately
assessable. Historical/stale evidence may be retained diagnostically, but never
becomes accepted through the stale worker mutation path.

Contract revision is an atomic fence: append the revision, invalidate pending
decisions/submissions for dispatch or acceptance, request interruption of
affected executions and retain their reservations. Existing immutable evidence
stays readable. Reuse under the new contract requires explicit revalidation and
a new submission/assessment; a stale approval, answer or result is rejected.
Late protocol replies resolve only the exact still-open request and contract.

## Bounds and implementation seam

Use explicit validated limits initially: command payload 256 KiB; individual
instruction/intent/result summary 16,384 Unicode scalars; identifiers 256 scalars;
receipt 16 KiB; up to 64 artefact/criterion/dependency references per command;
checkpoint summary 4,096 scalars; at most 500 history rows through existing
keyset pagination. Reject excess, NUL and invalid identities; do not silently
truncate commands or acceptance evidence. Large outputs remain in approved
artefact storage with digests. Canonical payload encoding is versioned and
deterministic. An external request key is unique per execution and protocol
request, including the broker process generation because numeric protocol
request IDs can recur after restart. Duplicate delivery cannot create a second
question or answer. A resolution is durable before protocol delivery; delivery
acknowledgement is a checkpoint, so reconnect can reconcile an unanswered request
without silently inventing a second answer.

Contracts specify finite maximum turns, runtime per turn, wall-clock deadline,
recovery attempts, repairs, concurrent workers and total packages/checkpoints/
questions. Package turn counts, turn duration and deadline also limit that
package; the other counters apply to the aggregate outcome. Charge dispatch budget in the intent transaction
and charge retries/recovery monotonically; replay is free. Bound requested
limits and reject overflow. Budget exhaustion creates recoverable attention with
consumption and next decision. An operator budget increase is a contract revision.
Model/reasoning selection and protocol details remain in adapter profiles and
execution evidence, outside obligation transition rules.

Implement records and validation in a new engineering domain module and Store
extension. Keep lifecycle mutation in `store.rs`'s transition owner; make only
the narrow transaction helpers needed by that extension `pub(crate)`. Append a
new migration and manifest digest; never modify applied migrations. Emit
engineering history through obligation audit events so the existing global
event envelope and operator watermarks remain authoritative. Extend operator
task/topic projections with responsibility, contract, questions, acceptance and
next action without adding a second state store.

Deterministic tests must cover receipt replay/conflict, stale contract/lease
fences, dependency cycles and failure, result-to-supervisor atomic handover,
generic-route exclusion, cancellation with an unproven writer, offline result
import, repair deduplication, exact-revision acceptance and budget exhaustion.
Runtime/transport implementation follows the selected live calibration.
The bounded initial outcome-ID list has a 500-row cap and no cursor; canonical
audit history uses existing keyset pages. There is no generic resume command:
trusted cessation may import offline evidence or permit a new execution, while
unreaped reservations remain held. Ordinary post-reap imports currently retain
the same recovered-submission attribution as offline imports; protocol evidence
distinguishes their actual observation history.

Runtime startup failures retain a bounded, typed adapter explanation and park the
responsible obligation until changed evidence or configuration supports another
attempt. Verified cessation is retained even when the recovery budget is exhausted.
A trusted `not_started` proof covers the pre-manifest crash window and failed
workspace-lock acquisition; it does not invent a process-reaping observation.
An inadmissible offline result cannot roll back an otherwise valid cessation proof.
Dependency readiness follows immutable repair chains to the current accepted owner.
