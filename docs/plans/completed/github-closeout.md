# Bounded GitHub publication closeout

- Status: complete
- Delivery state: acceptance-complete
- Acceptance state: passed
- Acceptance evidence: [Deterministic qualification](../../supervision-evidence/github-closeout.md)
- Landing evidence: https://github.com/robchristie/bokkie/pull/36

Delivered through the existing Store/runtime/host adapter:

- [x] Creation receipts distinguish a found PR from applied text.
- [x] Workers can explicitly update the exact open PR with an observed-text guard.
- [x] Supervisors publish deterministic review/CI/merge closeout from retained evidence.
- [x] Durable intent freezes publication inputs; read-back handles lost acknowledgements without blind reposting.
- [x] Role, authority, cessation and exact merge guards remain enforced; publication is distinct from acceptance and cleanup.
- [x] Deterministic regression, backend and canonical verification pass; instructions and operating guidance describe the delivered contract.

Disposable Git and scripted GitHub responses establish the repaired behaviour
without model turns or live Pagefold mutation. No historical PR is edited and no
runtime deployment is included. GitHub metadata updates are not atomic against
concurrent edits; missing or ambiguous publication evidence remains unresolved.
