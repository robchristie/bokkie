# Isolated supervision fixture

Status: qualified at `381fc8186322e8429e585225c265ffc071b50fd9`.
[Run j](fixture-j.json) retains the passing live observations and identities;
[attempts](fixture-attempts.md) retain prior failures and infrastructure repairs.
Deterministic Store/protocol tests cover exact stale, cancellation, lease,
relationship and exhausted-budget cases; the live run proves the complete
question, restart, offline-result, independent-review, repair and acceptance journey.

## Question and exit condition

Can the implemented Bokkie runtime preserve an accepted engineering intent
across questions, incomplete delivery, process loss and repair, then accept
only concrete evidence for the current contract? The smallest product fixture
is a local arithmetic module with deterministic tests and a short README.
Inputs are synthetic, the repository has no remote, and its database, derived
evidence and mutable workspace are task-scoped. Evidence belongs here.

Exit only when the deterministic Store/protocol cases and a bounded live
supervisor/worker journey pass. A successful transport or worker completion
does not satisfy the exit condition. Pagefold dispatch follows this fixture.

## Product journey

Submit plain intent for a small Python module exposing addition and multiplication
of integers, with tests for negative and zero values and usage instructions.
Existing intent records the module name and local-only authority. The worker
asks a routine question whose answer is already present in that record;
Bokkie persists the question, schedules its supervisor and supplies the answer.

The fixture must force a first incomplete submission (multiplication or its
verification is absent). Record the injection explicitly as fixture setup.
Bokkie's supervisor inspects the exact artefact and checks, records rejection,
commissions linked repair and later accepts the repaired revision. A separate
independent engineering review remains distinct from product acceptance.
The fixture must not force a defect in the later Pagefold run.

## Recovery matrix

| Trigger | Required observation |
|---|---|
| Restart immediately after dispatch | Same durable execution/dispatch identity; no duplicate writer |
| Worker completes while controller is unavailable | Broker retains concrete result; restarted Bokkie submits it for acceptance |
| Lose a command acknowledgement | Identical replay returns durable receipt; no duplicate package, answer or repair |
| Expire/replace a worker | Old result cannot advance authoritative state; prior writer stops or replacement is isolated |
| Revise intent during execution/acceptance | Old contract result/decision cannot complete the revised outcome |
| Cancel while termination is uncertain | Visible reconciliation responsibility and workspace reservation survive |
| Reap actual containment boundary | Escaped descendant cannot write; only then release same-workspace ownership |
| Request new external publication authority | One precise human escalation, no publication and no silent retry |
| Exhaust finite recovery budget | Durable recoverable state names consumption and next decision |

Use deterministic clocks and protocol fixtures for exact crash windows, stale
messages and lost acknowledgements. The bounded live journey must use actual
Codex supervisor/worker turns through the implemented runtime. Retain exact
source/fixture identities, outcome/contract/execution/question/submission/repair/
acceptance identities, test outputs and observation hashes.

## Interventions

Outer infrastructure repairs remain authorised, but record the failed condition
and repeat the affected journey. Routine supervisor answers or worker briefs
supplied manually by the outer agent invalidate that qualifying journey.
During Pagefold qualification, the outer agent observes and collects evidence;
it cannot substitute manual orchestration for Bokkie and report success.
