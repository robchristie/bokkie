# Conversation home

## Product direction

Bokkie is one assistant that helps the operator think, organise work and keep
commitments. The first screen invites a message. The operator should not need to
select an agent, execution adapter or engineering workflow before explaining an
outcome. The durable scheduler remains responsible for accepted tasks.

The useful lesson from [Dots](https://openai.com/index/introducing-dots/) is the
primary assistant relationship, visible ongoing work and deliberate review of
consequential actions. The supplied chat reference also suggests a calm reading
surface and compact work receipts. Bokkie's navigation follows work and context;
specialist identities belong in configuration and activity details.

## Information architecture

- **Bokkie:** the default conversation home, with recent conversations available
  without requiring a new agent identity. A persistent composer stays reachable
  as messages and task history grow.
- **Tasks:** find configured work and drafts, see timing and the latest result,
  and open a task in conversation. Operational attention and legacy task details
  remain accessible with their existing authority checks.
- **Today, Upcoming, Needs your input:** filtered views of that same catalogue,
  reachable from Home and Tasks. Today includes outstanding due work and today's
  completed results. Upcoming starts after the operator's current calendar day;
  task-specific zones remain visible. Needs your input includes drafts, candidate
  revisions and unresolved execution or notification attention, including older
  delivery failures. It does not enumerate every generated occurrence as a new task.

The global Tasks control opens that catalogue from both Home and Needs attention.
Advanced tools retains the legacy execution-record ledger for operational work;
it is not presented as another Tasks destination.
- **Settings (next extension):** appearance, agent roles and project workspaces
  in the target design. Only settings supported by a persisted backend contract may be
  presented as effective editable configuration.

On a wide screen, the conversation occupies the main reading column; selected
task details occupy a secondary panel. On narrow screens, navigation and task
details use explicit open/back controls, leaving the conversation and composer
the available width. Navigation retains unsent text and selected context.

## Conversation and task states

The empty home uses a short invitation and useful examples grounded in available
capabilities. It does not advertise email, research or project execution as
enabled when those adapters are absent. Disconnected or unavailable runtimes
explain the condition while preserving readable history and task access.

Messages are selectable, bounded in reading width and visually distinguish the
operator from Bokkie. Progress says what is happening. Technical IDs and process
metadata remain available through disclosure.

A proposed task shows its purpose, instructions, timing, destination and effects.
One clear action reviews the exact change; confirmation remains an explicit
operator action. Natural-language assent never bypasses the existing review
contract. An ambiguous task match requires selection.

A new reminder uses the one configured email destination. “Every weekday at 9”
needs am/pm clarification; an omitted zone uses Australia/Adelaide, while an
explicit zone is preserved. Review shows the concrete next dates and exact
reminder text. Without notification configuration, reminder activation is blocked
and the missing destination is explained. Existing local notes retain their
in-app behaviour.

After confirmation, the interface shows a compact saved-change receipt. The
completed review leaves the main reading path. A task can be scheduled, running,
waiting, paused or completed independently of that receipt. A completed result
is prominent and selectable; old runs and provenance remain available in detail.
An unrelated change must not erase a draft or invalidate an unaffected review.
Restart, session change and revision conflicts retain existing fail-closed rules.

Occurrence completion and notification delivery are distinct. Results show relay
acceptance, a scheduled retry or actionable delivery attention. Relay acceptance
does not prove inbox receipt or a phone alert. An uncertain send offers explicit
review to resolve without resending or resend with a stated duplicate risk; the
confirmation identifies the saved text and destination. Transport diagnostics
and stable delivery identities belong in disclosures.

## Agent roles and development hand-off

The intended configuration has one conversational orchestrator and optional
specialist role profiles. Each profile selects a model, supported thinking level,
instructions, permitted tools and finite execution limits. Escalation may select
an adviser such as Astra when an ordinary role cannot make progress. Model names
and tuning belong to role profiles rather than task forms or the scheduling
kernel. Delegation is visible in activity when relevant, without requiring the
operator to maintain separate chats with every worker.

The current conversation runtime has one deployment-owned profile. Editable
role profiles, delegation and escalation require a separate versioned persistence,
capability-validation and runtime-consumption contract. Visual controls alone
must not claim that these capabilities exist. The eventual settings editor
should validate supported model/effort choices server-side and preview changes
before saving. Pin a selected profile revision to each request, so retry retains
its original model, effort, permissions and limits. Escalation needs a defined
trigger and remaining budget; it does not acquire broader permissions.

Development work should produce a concise hand-off to the selected project
workspace: requested outcome, relevant context, constraints and a return link.
The project workspace owns its Codex development workflow. Bokkie records the
handoff and any supported returned status; it must not describe opening a link or
preparing instructions as an accepted execution. Existing engineering supervision
remains an advanced capability until a real workspace hand-off adapter is
qualified. This redesign does not transplant rob-codex-workflow into Bokkie.

## Qualification

Judge the design using a representative conversation, an inactive draft, a
current review, a saved change, a completed result and a recoverable failure.
At desktop and narrow widths the composer, current state and next meaningful
action must be clear without clipping or overlapping controls. Keyboard focus
and text selection remain usable. Task discovery, stale-review rejection,
restart recovery and existing operational actions must still work.

Use synthetic state for interaction development. Attribute browser evidence to
the actual candidate and open captured images before judging the visual result.
Keep model-backed capability changes and production deployment separate from
presentation verification.

The first conversation-home implementation and its observed limits are recorded
in [qualification](conversation-home-evidence/README.md).
