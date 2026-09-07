# Ordinary experience fixture

`bokkie-ui-fixture --variant experience` creates a small, disposable operator
day for reading-flow review. Its task descriptions are intentionally ordinary:
a support handover, roster approval, planning summary, calendar retry, import
warning, meeting notes and an obsolete reminder.

The fixture records pending, awaiting-approval, running, retry-scheduled,
attention, completed and cancelled activity through `Store` transitions. It
does not start a runner, contact an external system, register a gardener or
perform a real operator action.

Use it alongside the existing `full`, `empty`, `empty-inbox` and `large`
qualification variants. Those variants retain their adversarial purposes;
`experience` supplies the ordinary-reference fixture for presentation review.

Capture an ordinary attention item with the existing settled capture tool:

```sh
BOKKIE_APPEARANCE_FIXTURE=experience \
BOKKIE_APPEARANCE_CASES='[{"name":"experience","obligation":"check-import-warning"}]' \
node tools/ui-appearance-capture.mjs
```

This uses the ignored default output directory. Set `BOKKIE_UI_EVIDENCE_DIR` to
another candidate directory when retaining multiple comparisons. The manifest
records the actual fixture variant and source/artefact provenance. Build the
fixture binary and browser package first using the attention UI README.
