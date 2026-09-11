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
All complete attempts run all seven deterministic probes and fresh local preflight.
After a failure, record the changed relevant inputs and diagnosis, then pass the
relevant probe before a complete attempt. A failed probe never clears diagnosis.
The driver exposes no arbitrary live command or switch that can disguise a complete
fixture as a probe. Its current focused probes need no model turns. A future live
capability probe must use the same campaign reservation API with a small explicit
context envelope; it cannot reuse the complete fixture under a different label.

| Probe | Historical failure property exercised without a model |
|---|---|
| `qualification_driver` | Actual worker-command marker, complete acceptance observations, bounded deadlines and controller cessation |
| `campaign_admission` | Durable reservation, crash/restart, finite budgets and probe/repair admission |
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
reconciliation proves not-started status or verifies that the controller has ceased,
all Store outcomes are terminal, and all execution boundaries have ceased. The
driver holds a fixture lock and gives its controller a Linux parent-death signal;
a crash between controller spawn and its identity receipt remains unresolved.

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
Report schema version 2 separates coverage from available counters:

- `observed_contexts` counts distinct persisted thread identities.
- `context_inventory_complete` asserts coverage of all execution contexts only
  with explicit positive coverage evidence for every resolved model attempt and
  a matching durable inventory. `contexts_complete` retains that same strong
  meaning as a compatibility alias. False means completeness is unproved.
- `observed_token_metrics_complete` reports availability of `input`, `cached`,
  `uncached` and `output` counters across observed contexts only;
  `observed_telemetry_complete` combines those flags. These do not certify final
  runtime totals or coverage of unreported contexts. Empty observed sets have
  vacuously available counters; coverage still needs separate evidence.
- Existing `input_tokens_complete`, `cached_input_tokens_complete`,
  `output_tokens_complete`, `telemetry_complete` and the new
  `uncached_input_tokens_complete` require complete coverage as well as their
  counters. Known subtotals remain available regardless of these flags.
- Existing per-accepted-qualification ratios retain campaign-wide semantics and
  are null when coverage is unknown; the token ratio also requires complete
  input/cache/output telemetry, preserving its earlier gate.

The current broker cannot certify the total inventory: `context_limit` describes
`observed_events` enforcement with `unreported_children: unknown`. Collection
retains that reason once per execution, regardless of event replay. Missing
coverage metadata, including legacy observations with old completeness flags,
also leaves coverage unproved. A recorded verified pre-model fault with no
observed contexts retains known-zero usage. Reports regenerated from existing
ledgers apply these conservative rules without a schema migration; immutable
historical JSON reports retain their original values and require the linked
correction below when interpreted. No absent field implies complete coverage.

`reserved_contexts` and `charged_contexts` are admission-policy allowance units,
not observed model usage, tokens, billing or quota. `context_allowance_measure`
carries this distinction in machine-readable reports. The 240-context envelope
and reservation/refund policy are unchanged.

Response counts are observed agent messages or distinct cumulative usage updates,
a labelled lower bound rather than an invented exact model call count.

Initial context composition is measured in UTF-8/JSON bytes: role instructions,
dynamic tool schemas, command schema, outcome/history snapshot and
remaining envelope. Guidance/skill file bytes are a discovered-file inventory,
not proof that their full contents were injected into the prompt. These overlapping diagnostic fields must not be summed as
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

The [retained qualification](qualification-controls.json) passed on runtime and
qualification candidate `dc5c3a248f26ec7e8f3ddb7bec35f7d64b094129` after independent
review and canonical verification. All seven focused probes and installed Codex
0.154.0 preflight passed on that exact candidate. The complete fixture used eight
main-outcome executions and one authority execution, with three visible child
contexts: 12 observed contexts, one complete attempt and no live focused probes.

The run took 746.76 seconds. Available per-thread cumulative observations report
4,945,794 input tokens, 4,226,560 cached input, 719,234 uncached input and 18,346
output tokens. There were 81 observed response/usage-update observations under the
report's lower-bound definition. No unplanned outer-agent or human intervention,
preflight failure, suppressed retry or final-only infrastructure defect occurred.
The three intentional fixture injections remain separate. All boundaries were
reconciled and the accepted arithmetic checks passed. The campaign charged its
240-context reservation conservatively, leaving 510 reserved-context allowance.

The retained JSON includes byte composition, exact component/profile/environment
identities, criteria/review/acceptance evidence and hashes of local receipts. Four
outer implementation/review contexts supported this work package; their token
telemetry is unavailable and separate from fixture usage. Four early independent
review findings were repaired before model qualification began. Subsequent
candidate changes contain evidence and documentation only; the live-qualified
runtime and qualification component identities are unchanged. No comparable
savings or weekly quota-charge claim follows from this single observation.

Policy may be configured with `configure --limits policy.json --evidence decision.json`
only before the first reservation. `finish --evidence landing.json` requires a
passing final fixture and retained `reviewed_revision`/`landed_reference` fields.
Only then may `successor --next-campaign NEXT-ID --evidence next-change.json`
explicitly archive the terminal campaign and start the next work package. Old IDs
remain reportable and immutable. An exhausted or interrupted campaign cannot take
this path. These commands retain decisions; they do not perform review or merge.

## PR #28 telemetry correction

The [corrected interpretation](qualification-telemetry-addendum.json) references
original evidence hashes and recollected journal identities. It supersedes only
the historical completeness and ratio interpretation: 12 contexts were observed,
but the total context inventory is unknown. All observed contexts have the
available input/cache/output counters quoted above. Those are known subtotals;
they cannot establish complete campaign-wide totals or ratios. The original
qualification JSON, retained reports, terminal ledger and acceptance remain
unchanged. No new live qualification was run for this deterministic correction.
