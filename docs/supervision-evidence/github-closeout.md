# GitHub publication closeout qualification

Qualification on 14 September 2026 used the implementation at
`2be76f8f42249d0f1cd0abf5499c1b89e5e89392`; subsequent plan/evidence closeout does
not change the tested implementation. The owning delivery is
[Bokkie PR #36](https://github.com/robchristie/bokkie/pull/36).

## Question and boundary

Can Bokkie update stale PR text explicitly and retain attributable public
closeout without duplicate publication after an uncertain acknowledgement?
The smallest probe uses disposable standalone Git and scripted GitHub responses.
No model turn, live Pagefold mutation, runtime deployment or historical outcome
adoption was used. Recorded protocol tests establish adapter behaviour, not a
claim that a new autonomous Pagefold feature has already exercised it live.

## Results and owners

- `tools/check.sh`: passed. Python discovery ran 180 tests. The Rust library
  suite passed 275 tests with its two existing ignored boundary probes; canonical
  Python fixtures also exercise their selected Rust boundary commands. The CLI,
  fixture binary and integration suites passed (3, 2 and 14 tests respectively).
  Plan/toolchain governance, Clippy and formatting passed.
- [Adapter fixtures](../../tools/tests/test_github_delivery.py): all 36 tests
  passed, including the original stale-description failure, explicit updates,
  read-back, literal multiline/Unicode text, moved head, changed text, lost
  acknowledgements, exact closeout evidence, duplicate/altered/foreign comments,
  bounded enumeration, and publication after checkout has returned to main.
- [Store integration](../../src/store/engineering.rs) proves publication role
  separation, durable intent replay after reopen, blocking of overlapping
  operations, verified-merge/cessation prerequisites and separation from outcome
  acceptance. Existing grant, stale lease/revision and cancellation fences remain.
- [Runtime integration](../../src/engineering_runtime/reuse_tests.rs) proves
  model arguments cannot substitute review/CI evidence, mismatched retained
  identities are rejected, and closeout read-back uses frozen intent inputs after
  cancellation or contract revision. Empty host responses leave publication pending.

The focused `github_delivery` probe includes these adapter and backend checks.
Exact committed-candidate probe results, independent review, pre/post-merge CI
identities and cleanup receipts are retained on the owning PR.

## Limits

PR text updates use observed-content and head checks, not an atomic GitHub
compare-and-swap. A detected head race remains uncertain. Closeout enumeration
supports fewer than 100 comments and refuses ambiguous or altered markers.
An absent read-back result never authorises blind reposting. Publication remains
an explicit delivery operation; it does not complete product acceptance or
cleanup. The supervisor instructions require publication before normal final
acceptance. The local-only profile and historical completed outcomes are unchanged.
