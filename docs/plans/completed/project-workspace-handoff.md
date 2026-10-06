# Project workspace hand-off

- Status: complete
- Delivery state: acceptance-complete
- Acceptance state: passed
- Acceptance evidence: [Qualification results](../../handoff-evidence/README.md)
- Landing evidence: https://github.com/robchristie/bokkie/pull/54
- Landed pull requests: none before this package
- Production deployment: separate authority; source delivery only

## Outcome and boundary

Bokkie prepares, reviews, saves and copies a concise brief for an existing project
workspace. Codex owns the actual desktop catalogue and host connections; Settings
holds an operator-maintained address book referencing those identities. The
receiving workspace owns execution and its established guidance/workflow.

The qualified transfer route is manual project selection and paste. No supported
external project-opening/prompt-transfer URL was established. Opening instructions,
copying, reading and saving do not activate a task or worker or grant permissions.
Saved revisions snapshot the destination and preserve exact return links. Notes
record operator-entered results; action reports never imply execution acceptance.

## Acceptance

- [x] Small Settings surface registers stable identities, readable names, host/path
  addresses, optional Codex references and short selection context.
- [x] Server validation rejects invalid identities, paths, schemes and stale selections
  without mounting repositories, credentials or administration sockets.
- [x] Bounded conversation drafting retains relevant outcome, decisions, constraints,
  acceptance and supplied links, with explicit ambiguous project selection.
- [x] Review/edit/save preserves unsent text and entered briefs through navigation.
- [x] Immutable saved snapshots and explicit revisions are atomic and replay-safe.
- [x] Browser copy and readable native selection/keyboard fallback transfer all text.
- [x] Manual opening guidance and an opening-failure report establish only known facts.
- [x] Exact return navigation, attributed result notes and restart persistence pass.
- [x] Two contained provider turns establish representative language interpretation;
  the unrelated synthetic private detail is omitted and save/replay add zero calls.
- [x] Existing task, scheduling, approval, login and containment contracts pass checks.
- [x] All 13 desktop/narrow candidate screenshots were opened and judged.
- [x] Deployment changes and stopped-state schema17 rollback considerations are documented.

## Evidence and limits

[Retained results](../../handoff-evidence/README.md) include 219 Python tests, 369
backend library tests and the complete integration suites, 119 UI Rust/30 Node
tests, native/Wasm builds, lint/format, the two-call deterministic browser journey
and two actual provider turns through qualified Codex0.160.0.

Linux Chromium owns the browser qualification. Native clipboard delivery has no
read-back acknowledgement; other browser/OS combinations are unqualified.
Automatic execution, callbacks, status synchronisation and publication remain
outside this package. The owning pull request carries independent review, CI and
landing evidence. Production activation follows the separately authorised
[deployment procedure](../../deployment.md).
