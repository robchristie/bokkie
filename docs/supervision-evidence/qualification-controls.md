# Qualification controls and comparison protocol

This work package owns qualification admission and measurement. Historical fixture
and Pagefold evidence remains historical; it selects regressions and allowances,
not current-candidate acceptance. The user supplied baseline observations of 91
fixture contexts and approximately 3.34 million uncached first-response input
tokens out of 4.15 million fixture input tokens. These figures were not measured
by this change. Tokens do not establish a weekly quota or billing charge.

## Owners and stages

`tools/engineering-runtime/preflight.py` owns no-model compatibility probes and
local installed-runtime inspection. It calls production broker validation and
source capture, and exact named Rust/Python regressions against production
journal, submission and reply functions. `tools/qualify-engineering.py` owns stage
progression; `tools/qualification_campaign.py` owns durable qualification
reservation/accounting. Neither defines obligation transitions: Store remains
authoritative for dispatch, cessation, repair and acceptance.

The stages are preflight → focused probe → complete fixture → application dogfood.
All complete attempts run all five deterministic probes and fresh local preflight.
After a failure, record the changed relevant inputs and diagnosis, then pass the
relevant probe before a complete attempt. A failed probe never clears diagnosis.
The driver exposes no arbitrary live command or switch that can disguise a complete
fixture as a probe. Its current focused probes need no model turns. A future live
capability probe must use the same campaign reservation API with a small explicit
context envelope; it cannot reuse the complete fixture under a different label.

| Probe | Historical failure property exercised without a model |
|---|---|
| `config_schema` | MCP table preservation, unquoted server keys, canonical concurrency field, effective configuration mismatch |
| `child_review` | Root attribution, child final answer and exact successful child turn; wrong or missing provenance rejected |
| `submission_binding` | Invented command observations and changed source cannot become valid submission evidence |
| `encoded_paging` | Nested JSON escaping, binary paging, oversized responses and malformed requests remain bounded and reconcilable |
| `source_boundaries` | Full capture above the old journal-size ceiling; precise file/count/byte failures and unsafe paths |

The sanitised regression inputs preserve triggering event/payload structures and
boundary sizes. No full historical transcript, account configuration or private
source is copied. Tests run in ordinary `tools/check.sh`; CI needs no account.

Local preflight starts the installed contained app-server, initialises it, reads
its effective configuration, creates ephemeral threads with the production tool
schemas and settings, and inspects guidance/skills. A protocol allowlist rejects
`turn/start`. Both supervisor and worker profiles are checked. Source selection
uses the same runtime capture function and limits as command observations.

Receipts separate full runtime/component, effective environment and workspace
identities. They include the Codex launcher digest and reported version, profile
hash, tool/instruction identities and capture totals. Workspace source changes
invalidate workspace evidence; unrelated source changes do not invalidate the
effective environment identity. Guidance, effective configuration, tool schemas,
broker/preflight implementation or executable changes invalidate environment
identity. Full component identities bind deterministic probes. Moving identical
fixture guidance does not disguise an unchanged configuration failure.

Admission always re-observes local conditions; it does not skip checks on the
basis of a cached timestamp. Receipt files are compact evidence, not bearer
permissions. Generated replies and later source/journal growth remain subject to
production limits and recovery. A launcher wrapping a native executable is bound
by its launcher digest and reported Codex version, not an independent native
binary digest.

## Durable allowance and recovery

The canonical registry is `qualification/campaign.sqlite` under the repository's
Git common directory, shared by its worktrees. No CLI ledger-path override exists.
Every fixture root is bound to this registry, campaign and immutable attempt ID.
Changing a fixture directory, campaign ID or stage cannot replenish an active
campaign. SQLite immediate transactions arbitrate concurrent reservation. A
single-use launch claim is durable before the controller starts. Reservations
are never refunded; crashes before or after launch remain unresolved until
reconciliation proves not-started status or Store verifies cessation.

Initial configurable defaults are three complete fixtures, two live probes, one
application dogfood slot, 750 reserved contexts and 240 contexts protected for a
final complete fixture; diagnosis begins after two consecutive complete failures.
The complete envelope is `(24 main + 24 authority outcome turns) × (3 observed
contexts per execution + 2 concurrent child slack) = 240`. Historical fixture h
used 19 main executions; j used nine plus one authority execution. The unchanged
24-turn profile therefore admits both known successful paths with conservative
child and authority headroom. Three complete envelopes leave 30 contexts for
small live probes. A dogfood slot shares that finite total, rather than adding
hidden allowance. These are adjustable initial policy choices, not claims of
future usage or guaranteed stochastic success.

The qualification controller sets a process-scoped observed-context guard in each
broker. It counts unique root/child identities and stops through normal namespace
reaping on excess. Replay does not consume another context. Child notifications
arrive after creation, so reserved concurrent slack covers observation latency;
unreported runtime children remain an observation limitation. The outer runner
also checks aggregate observations. No persistent service or global profile is
changed, and workflow tools and Astra/effort remain enabled within the bound.

Normalised failure classes and relevant component fingerprints suppress unchanged
repetition immediately. Any retry after failure requires changed relevant inputs,
a recorded repair and a later passing relevant probe. Two consecutive complete
failures additionally require explicit diagnosis. Exhaustion or uncertainty leaves
an incomplete campaign with retained evidence and the next diagnosis/reconciliation
action. Deterministic investigation remains available and free of live allowance.

## Measurement and the next three changes

Each observation is bound to an execution/thread. Usage is the latest cumulative
per-thread counter, not the sum of replayed events; child totals are not added to
a parent aggregate. Missing input/cache/output telemetry is unknown. Reports show
known subtotals separately from complete per-accepted-qualification ratios.
Response counts are observed agent messages or distinct cumulative usage updates,
a labelled lower bound rather than an invented exact model call count.

Initial context composition is measured in UTF-8/JSON bytes: role instructions,
guidance files, dynamic tool schemas, command schema, outcome/history snapshot and
remaining envelope. These overlapping diagnostic fields must not be summed as
independent buckets. Source contents and retained evidence retrieved later through
tools are not part of the directly assembled initial prompt; initial references
are in the snapshot. Runtime-injected tool/schema/prompt material and precise
per-component token attribution remain unknown. No sensitive prompt is retained
solely for measurement.

For each of the next three supervision changes, retain a separate completed
campaign report and exact accepted source/evidence identity. Compare fresh
contexts and uncached input per accepted outcome, complete attempts/live probes,
pre-model failures, suppressed unchanged repetitions, elapsed time, outer-agent
and human interventions, and defects first discovered during complete qualification.
Keep planned fixture restart/offline/authority injections separate from corrective
interventions. Record workload/profile/runtime differences and missing telemetry.
There is no demonstrated savings claim until comparable observations exist, and
no automation or requirement to wait for those future changes.

## Commands

From the Bokkie checkout, create a new synthetic root outside the repository:

```sh
python3 tools/qualify-engineering.py prepare --campaign CHANGE-ID --runtime-root /absolute/new-fixture
python3 tools/qualify-engineering.py preflight --campaign CHANGE-ID --runtime-root /absolute/new-fixture
python3 tools/qualify-engineering.py probe --campaign CHANGE-ID --runtime-root /absolute/new-fixture --probe encoded_paging
```

After a repair, the probe record must name the prior attempt and concrete diagnosis:

```sh
python3 tools/qualify-engineering.py probe --campaign CHANGE-ID --runtime-root /absolute/new-fixture \
  --repair-of FAILED-ATTEMPT-ID --repair-note 'Describe changed inputs, failure and acceptance criterion' --probe encoded_paging
```

A complete attempt requires a clean committed candidate. It automatically repeats
mandatory checks and reserves allowance before starting any model turn:

```sh
python3 tools/qualify-engineering.py complete --campaign CHANGE-ID --runtime-root /absolute/new-fixture --run-live --final
python3 tools/qualify-engineering.py report --campaign CHANGE-ID
python3 tools/qualify-engineering.py reconcile --campaign CHANGE-ID --runtime-root /absolute/retained-fixture
```

Reconciliation reads retained Store cessation; it never launches a replacement or
claims an interrupted journey passed. If cessation is unresolved, use the existing
runtime's private database/profile reconciliation path and retain the reservation.
Do not delete spools, writer markers or ledgers. All acceptance evidence remains
with the retained fixture root; canonical checks and independent review remain
required before landing.

## Application repetition decision

Pagefold product code, intake, reader journeys and acceptance rules are unchanged.
The affected paths are runtime compatibility, protocol evidence, telemetry and
qualification admission; the complete synthetic fixture exercises them. Pagefold's
full build and journey need not repeat. Its prior acceptance remains historical,
and no new Pagefold acceptance claim is made.

## Current candidate evidence

Pending final candidate verification and complete live fixture. The active plan
owns progress until those observations are retained here.

Policy may be configured with `configure --limits policy.json --evidence decision.json`
only before the first reservation. `finish --evidence landing.json` requires a
passing final fixture and retained `reviewed_revision`/`landed_reference` fields.
Only then may `successor --next-campaign NEXT-ID --evidence next-change.json`
explicitly archive the terminal campaign and start the next work package. Old IDs
remain reportable and immutable. An exhausted or interrupted campaign cannot take
this path. These commands retain decisions; they do not perform review or merge.
