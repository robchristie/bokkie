# Engineering qualification campaign controls

- Status: complete
- Delivery state: acceptance-complete
- Acceptance state: passed
- Acceptance evidence: [Controls, commands and comparison protocol](../../supervision-evidence/qualification-controls.md), [exact fixture and usage evidence](../../supervision-evidence/qualification-controls.json)
- Landing evidence: https://github.com/robchristie/bokkie/pull/28

## Accepted outcome

Runtime-owned production-path preflight and seven no-model probe groups detect
representative September configuration, journal/review, submission/source and
encoded-payload failures. Local app-server inspection uses the actual schemas,
effective settings and source-selection limits without starting a model turn.
Component, environment and workspace identities are separate; admission always
re-observes local conditions instead of trusting a stale cached receipt.

The qualification runner owns explicit stages and a durable campaign registry
shared across fixture databases and Git worktrees. It reserves allowance before
launch, fences controller death, retains ambiguity, suppresses unchanged failures
and requires relevant changed-input repair/probe evidence. Finite policy protects
final qualification headroom. Store remains the sole obligation lifecycle owner.

Per-thread telemetry preserves cumulative/replay/missing-data semantics and labels
byte attribution and unknown runtime-injected content. Reports support the next
three supervision changes without requiring an automation or asserting savings.
Initial policy was selected before any live reservation; bootstrap model use was
zero and adoption did not reset consumed activity.

## Verification and decisions

| Increment | Owner / consumer revision | Result | Evidence |
|---|---|---|---|
| Production preflight, probes and durable admission | `dc5c3a248f26ec7e8f3ddb7bec35f7d64b094129` | Canonical checks; 86 Python tests; 254 Rust library tests; all seven focused probes | Qualification evidence above |
| Installed runtime compatibility | same candidate / Codex 0.154.0 | Supervisor and worker profiles, actual schemas, guidance and complete workspace capture passed with zero model turns | Hashed preflight receipt in evidence |
| Complete fixture | same candidate / fixture `d15294c1f8dfd932e57ab2e1a9dbbd082c4f560a` | Restart/replay, routine answer, offline completion, independent review, linked repair, separate acceptance, authority escalation and cessation passed | Exact outcome `773375ea-1320-4f2c-af76-9fb291ac46ea` |
| Evaluation and dogfood decision | unchanged qualified components / final documentation | One complete attempt, zero live probes, 12 observed contexts, 719,234 uncached input tokens; no unplanned live intervention | Machine-readable report and comparison limitations |

Early independent review found four machinery defects; all were repaired and
re-reviewed before the first complete attempt. Final evidence/documentation does
not change qualified runtime or runner inputs. Exact-head landing evidence belongs
to PR 28.

Pagefold product and acceptance paths are unchanged. The synthetic fixture covers
the affected runtime paths, so its full build/journey does not repeat. Earlier
Pagefold and fixture evidence remains explicitly historical.

## Retained limits

Observed child notifications can arrive after creation; finite reservations include
concurrency slack and unreported children remain unknown. A controller spawn/receipt
crash gap remains conservatively unresolved. Missing token evidence suppresses
complete ratios, and no exact billing/quota charge is inferred. Supplied historical
baseline and current all-response measurements are not directly comparable.
The configured Astra/effort, engineering workflow, worker permissions and Store
safety boundaries remain. No global configuration, private document ingestion,
live Bokkie database, persistent service, deployment or publication was involved.
