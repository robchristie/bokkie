# Legacy process shutdown qualification

Status: bounded queue-finalisation repair in the current evidence-report package.
Canonical check log `/tmp/bokkie-workspace-probe/command-log-rroh4q6g/` records
continuous-output finalisation hanging for 454.537 seconds before the owner stopped
that exact test with a pidfd signal. The direct child and all descendants were
observed absent; three remaining test threads waited on futexes. Existing
source had bounded reader/writer notification channels, a time-limited drain,
then blocking joins. A producer can remain blocked after receiving stops.

Question: can finalisation drain both queues while joining finished workers,
without changing output evidence, interruption classification or group signals?
Smallest probe: deterministic saturated channels and idle stdin receiver, followed
by existing output-limit, cancellation and normal-completion process tests.
Evidence owner: executable process regressions and private attributed logs.
Exit: all workers join, complete chunk bytes reach Capture, first error survives,
and the canonical candidate check passes. No timeout, skipped test or actor
completion sentence substitutes for this proof.

One bounded follow-up remains: preserve a credible original-group identity
through leader-exit/Drop cleanup. Reaping the leader releases the identity anchor;
a stored numeric PGID, `kill(-pgid,0)` and a direct-child pidfd do not establish
that a later group is the originally owned group. Do not add unconditional late
signals. Consider observing exit without reaping until group cleanup, or the
established containment owner; prove no signal after ownership is relinquished
and actual descendant cleanup, including the scope of escaped `setsid` work.
The new workspace host's outside subreaper already owns its writer boundary.
This follow-up belongs with legacy process ownership during consolidation.
