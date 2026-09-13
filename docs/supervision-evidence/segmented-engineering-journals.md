# Segmented engineering journal qualification

This change replaces the per-execution journal ceiling for new brokers with
bounded segments, deduplicated source observations and external large-output
blobs. It changes adapter storage, not Store lifecycle or delivery authority.

## Deterministic qualification

`tools/check.sh` covers the Python suite, governance/toolchain checks, all Rust
targets, Clippy and formatting. The `journal_storage` focused probe in
`tools/engineering-runtime/preflight.py` selects production-path storage tests
and Python-to-Rust replay without starting a model turn. Runtime identity now
includes the Rust journal module and its Python regression suite.

The self-contained cases establish:

- Every source observation retains command/phase identity; equal payloads share
  one immutable blob. Inline and referenced observations both reject checks
  against older source revisions.
- Large Unicode/CRLF command output round-trips as exact bytes. Storage and
  retained evidence contain references rather than repeated expanded payloads.
- A real OS fake app-server peer crosses segments in one process and finishes
  normally; rollover does not cause worker replacement.
- Python-written segments replay in Rust. Reconciliation retains ordered segment
  digests and unique payloads, pages their exact bytes and records cessation once.
  It does not grant acceptance merely because the worker ceased.
- Segment seals, sequence gaps, missing/corrupt payloads, symlinks, mixed formats,
  torn tails, byte/event/blob limits and rollover crash points fail conservatively.
  Read-only polling ignores only an incomplete active tail and snapshots file
  length before reading, so concurrent appends belong to a later observation.
- Partially written unreferenced blobs count towards storage bounds but do not
  invalidate active polling. A broker reopening for append validates their
  hashes; referenced blobs always require exact hashes and lengths.
- Legacy journals remain readable and unmodified. Telemetry handles both formats
  without counting an execution twice; cumulative usage remains idempotent and
  malformed evidence cannot establish fixture acceptance.

## Historical Pagefold replay

The interrupted navigation worker's retained input was read only:

- Execution: `6c912886-0e40-4158-9fa1-1ced04965ea5`.
- SHA-256: `e3c58bec32707b8b95ecb54a2ce30c4355e959fa8d4c9edea2b6fb3e2844c439`.
- Original: 1,095 events / 16,631,416 bytes.
- Temporary version 2 replay: 3,798,074 event bytes across four segments with a
  deliberately smaller 1 MiB segment threshold; five unique blobs / 417,233 bytes.
- Total event and payload bytes: 4,215,307, approximately 75% less than the
  original, excluding the small manifest/filesystem overhead.

All hydrated events compared equal to their original JSON values; source objects
and output strings remained exact. The input bytes were checked unchanged after
replay. This historical journal's reduction comes from source deduplication;
large-output externalisation is independently covered by synthetic cases.

Reproduce only when the retained local input is available:

```sh
BOKKIE_JOURNAL_REPLAY=/absolute/path/to/retained/events.jsonl \
  cargo test --locked --lib engineering_runtime::journal::tests::historical_replay \
  -- --ignored --exact --nocapture
```

The ignored replay creates and removes a temporary spool. Ordinary verification
has no dependency on private Pagefold files. Exact candidate identities, canonical
check results, independent review and CI are retained in the owning pull request.

## Limits

No live Codex supervisor/worker turns or fresh qualification campaign were used.
This establishes the changed storage contract, not another live Pagefold delivery.
New executions retain aggregate caps of 128 MiB events, 32,768 events, 64 segments
and 128 MiB / 4,096 blobs. Each segment remains bounded to 16 MiB and each payload
to 2 MiB. Exhaustion still requires cessation and bounded recovery; it is not
silently converted into acceptance or an unlimited execution allowance. Legacy
in-flight brokers retain their original limits. Model/profile settings, deadlines,
source-capture limits and the existing gardener boundaries are unchanged.
