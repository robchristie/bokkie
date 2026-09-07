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
