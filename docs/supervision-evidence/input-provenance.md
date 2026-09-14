# Starting-input provenance during evidence reuse

The addressable Pagefold application campaign exposed an applicability defect.
Bokkie delegated the initial README as an immutable input reference, then the
worker legitimately documented the feature in that file. Canonical and browser
checks bound the final source, and independent review passed. Registration
succeeded, but discovery and submission re-inspected the initial README against
current workspace bytes and rejected every validation. The original worker
reached its deadline; one continuation preserved its evidence without repeating
checks. No Pagefold acceptance or successful delivery is claimed by this record.

The outer operator owns this Bokkie infrastructure repair. Pagefold design,
implementation, calibration, browser tests, independent review and delivery remain
inside the application runtime. No campaign allowance or historical record changes.

## Bounded qualification

Question: can an input edited before validation retain original provenance while
reuse still rejects changes after validation? The smallest probe extends the
production runtime fixture with an actual starting input, changed before the
successful command. Evidence owner: this record and the owning PR; exit requires
same-package continuation, scoped submission and review coverage to pass with
unchanged command/source identities, while later changes and corrupt evidence fail.

The regression failed before the fix with no eligible validation. It passes
afterward with inline and segmented observations, file and clean Git artefacts,
replacement-worker reuse, and the existing original-worker fencing and advisory
review-coverage checks. Later source edits, changed package declarations and
corrupt original input blobs are rejected. A separate ignored-input fixture
proves that a file absent from captured source must still match its original live
bytes even when the captured source remains unchanged.

For a file present in the unchanged command source snapshot, reuse now verifies
the retained starting blob's digest and length instead of re-reading it as though
it were the submitted revision. The immutable package binding and source snapshot
still apply. Files outside capture and Git inputs retain live/exact inspection.
No schema, Python broker/environment identity, worker profile, model settings or
source-capture limits change. Existing valid observations need no re-registration.
The controller must restart to load the Rust repair; brokers need not be replaced.

Canonical verification: `RUST_TEST_THREADS=2 tools/check.sh`. The HTTP/UI/toolchain
and CI surfaces are unchanged, so no new UI journey or broad live qualification
fixture is required. Independent review and exact pre-/post-merge CI belong to the
owning PR. The affected application will resume ordinary discovery and advisory
submission under the reviewed merged runtime, preserving original provenance.
