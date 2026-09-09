# Isolated fixture attempts

This is qualification evidence, not a claim of product acceptance.

| Attempt | Exact source | Result and retain/reject decision |
|---|---|---|
| 20260909-a | `7b40bcc79e053598a3614ef1d63f26f367c43af9` | Rejected before a model turn. App-server rejected an MCP override with an invalid transport. Twenty-four bounded supervisor dispatch attempts exhausted the outcome turn budget. The outer agent diagnosed infrastructure; no routine supervisor answer or worker instruction was supplied. HTTP cancellation completed and every execution had verified cessation. |

Historical retained synthetic evidence owner:
`/nvme/development/bokkie-supervision-fixture-20260909-a` contains the SQLite
records, private broker journals, exact profile, fixture revision
`bab4e55a347449d6c3981d8144e9176b9abbd111`, journey and controller logs. Its profile
SHA-256 is `25ff72c7599171f6442140e19e3a1d5810c610ad8e4dbccc8617cffa7022329a`.
No source application or private knowledge was involved. The next run must use
a new database and fixture after startup/retry repairs; this attempt does not
establish any live supervision acceptance criterion.

Independent review of that same source also identified source-to-validation
binding, cancellation/import ordering, repaired dependency readiness, package
time limits, the pre-manifest crash window, HTTP intake deadlines and missing
fixture progress events. These are being repaired with regression coverage before
repeating the live journey. Detailed review belongs with the eventual pull request.
