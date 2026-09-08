# Repair the first live gardener runtime

- Status: active
- Reorientation budget: 100
- Landed pull requests: none
- Next action: verify descriptor and offline-cache repairs, then review and land one candidate.

## Current phase

The first live inspection and UI approval succeeded. Implementation created
candidate `880eb9e53978520ac44994505cd1c2cbe69c41b9` from
`d0339989d2e85ce60cc16fe2ceade3a6c391a367`, but offline checks could not resolve
Polyorama. No push or PR occurred. API/CLI disagreement additionally exposed
lost SQLite locks after the credential reader vacated descriptor zero.

A preserved database/WAL snapshot passed integrity and foreign-key checks and
matched the live API. Graceful shutdown checkpointed the original database;
the UI-only service now shows the retained attention condition correctly.
Operational evidence remains in the operator's private cycle state directory.

## Bounded repair

- Consume the one-shot credential without leaving descriptor zero available
  for SQLite; preserve credential confinement and non-dumpability.
- Expose only the dedicated Cargo Git cache read-only to candidate checks,
  alongside the existing registry cache. Keep checks offline and isolated.
- Retain deterministic regressions and real sandbox/paired service probes.
- Document required offline cache provisioning and the descriptor invariant.

## Acceptance

- Credential-bearing startup preserves cross-process SQLite visibility.
- Existing credential isolation checks continue to pass.
- A cached Git dependency resolves in the network-off candidate sandbox,
  and the cache remains read-only.
- Canonical backend checks pass; independently review and land the repair.

The original retry-delay proposal remains approved. Its later rerun is a
continuation of the operator cycle, not evidence that these repairs have
already qualified a complete live implementation/publication cycle.
