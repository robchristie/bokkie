# Runtime and trust hardening programme

- Status: active
- Owner: `bokkie`
- Review source: `goal-objective.md` supplied 2026-09-04
- Baseline: `12f5c65901d07248d152ebf80548116cf0c7b040`
- Target repository: `robchristie/bokkie`
- Reorientation budget: 200
- Landed pull requests: #5, #6, #7, #8, #9, #10, #11, #12, #13, #14
- Next action: obtain explicit human approval to squash-merge the exact reviewed P6b Apache-2.0 candidate, then run P6c exact merged-main qualification
- Last updated: 2026-09-05

## Outcome and terminal rule

Bring the live execution, credential, publication, database and projection
boundaries up to the obligation kernel's durability standard. The programme is
complete only when R1–R12 below are satisfied, superseded or explicitly
excluded by the necessary authority. P6c must qualify the exact merged `main`
head; an internal package landing is progress, not programme completion.

## Scope and authority

Ordinary code, tests, documentation, refactoring and CI changes may land under
standing reviewed-merge authority. The programme does not deploy or restart
Bokkie, install a service, exercise production credentials, merge a
gardener-created pull request, publish a release or perform destructive data
work. A licence grant and live branch-protection or repository-settings change
are respectively rights and access-control decisions that require explicit
human authority. Absence of either mutation is not approval to perform it.

## Current R1–R12 capability assessment

This assessment is against merged `main`
`bfb2104c55eaec6b01913c7ba5e97fba058bc77a` (tree
`87d469076bf1e3a4a996958625c2434a96411a98`, schema v9/API v1).

| Row | Current disposition | Evidence owner |
|---|---|---|
| R1 | Satisfied: bounded lease renewal, same-time no-op and expired-work projection | P1/PR #6 |
| R2 | Satisfied: supervised process tree, deadline, cancellation, output and typed failure boundaries; descendant fixture repaired | P1/PRs #6–#7 |
| R3 | Satisfied: explicit least-authority child environments, executable identity, Git configuration and credential revalidation | P2/PR #8 |
| R4 | Satisfied: candidate evidence gates draft-to-ready promotion and exact PR heads; CI is secret-free and unprivileged | P2/PR #8 |
| R5 | Satisfied: approval binds immutable proposal generation, observation and source commit | P3/PR #9 |
| R6 | Satisfied: startup-only immutable migration manifest, bounded DB execution and read-only diagnosis | P4a/PR #10 |
| R7 | Satisfied: schema-v9 global envelope, bounded consistent pages and incremental watermark | P5b/PR #13 |
| R8 | Satisfied: fair bounded failure-isolated ordinary and gardener execution lanes | P5a/PR #12 |
| R9 | Satisfied across P1–P5b: typed persisted process, trust, proposal, schema, failure, lane and projection identities have executable temporal/scale tests | PRs #6–#13 |
| R10 | Satisfied with human authority: runtime/API/schema identity, exact local-origin checks and per-process mutation secret | P4b/PR #11 |
| R11 | Satisfied: process, trust, execution-lane, events and pagination owners are split while Store retains lifecycle transitions and bounded domain reads | PRs #6–#13 |
| R12 | Partially satisfied: P6a landed executable plan truth, dual-toolchain contracts and CI reconciliation; P6b has applied the authorised live protection and holds the Apache-2.0 rights change for explicit human review | P2/P6 |

Already-satisfied guarantees retained by these dispositions include atomic
Store-owned lifecycle transitions, append-only audit events, generation/token
fencing, expired-claim recovery, network-off model sandboxes, canonical Git
origin checks, detached exact-head verification, immutable run identities and
conditional operator-state preconditions.

## Work packages

| Package | Outcome | State and terminal evidence |
|---|---|---|
| P0 | Re-baselined programme checkpoint | [PR #5](https://github.com/robchristie/bokkie/pull/5) landed as `e8d3218433f97c8dfc542c80d0ac813d283d86b5` |
| P1 | Bounded leases and supervised execution | [PR #6](https://github.com/robchristie/bokkie/pull/6) landed as `8154ca4a5bc4c3aa0356cc74e3874544b4231296`; fixture repair [PR #7](https://github.com/robchristie/bokkie/pull/7) landed as `60535e6ecd5f882c17cce8a2f2222d7a190faa1e` |
| P2 | Gardener runtime trust and publication controls | [PR #8](https://github.com/robchristie/bokkie/pull/8) landed as `9cf9329f374f545eccca596fd9df8a451a48a065` |
| P3 | Source-bound proposal generations | [PR #9](https://github.com/robchristie/bokkie/pull/9) landed as `9920006635c01cd266e6f2cd3a3546fe21747867` |
| P4a | Storage lifecycle, bounded DB execution and doctor | [PR #10](https://github.com/robchristie/bokkie/pull/10) landed as `87c8d42f1757c9656fbb010723b9e4bb1477dd69` |
| P4b | Local mutation access boundary and service identity | Human-authorised [PR #11](https://github.com/robchristie/bokkie/pull/11) landed as `4ecc30338c60bf66507558a68b96c65c34014c86` |
| P5a | Fair execution lanes and typed outcomes | [PR #12](https://github.com/robchristie/bokkie/pull/12) landed as `b8bda7877e63d73780f04dafe097fb7425cccc7a` |
| P5b | Incremental bounded projections and global event envelope | [PR #13](https://github.com/robchristie/bokkie/pull/13) landed as `d73f8417bbdce989c289ee3905da7ac28357964e` |
| P6a | Executable plan truth, repository reconciliation, dual toolchain contracts and CI governance | [PR #14](https://github.com/robchristie/bokkie/pull/14) landed as `bfb2104c55eaec6b01913c7ba5e97fba058bc77a` |
| P6b | Apache-2.0 licence and live `main` protection | Ruleset `22305031` version `48733815` is active with no bypass; the coherent licence candidate is human-held before merge |
| P6c | Aggregate exact-head qualification and programme closeout | Run only after P6b has an explicit apply-or-defer disposition; no deployment or release |

## Acceptance and integration proof

Every ordinary package receives deterministic focused tests, locked canonical
checks, independent exact-head review, applicable CI, squash merge and cleanup.
P6c reruns the plan linter, locked backend and UI checks, safe fake-process and
temporary-database journeys on exact merged `main`, then reconciles pull
requests, worktrees, local branches, remote-tracking references and live heads.

## Current phase

P6b has applied the specifically authorised repository branch ruleset and
verified its exact live and effective state. Ruleset `22305031` version
`48733815` targets only `refs/heads/main`, is active, has no bypass actor and
reports `current_user_can_bypass: never`. It requires pull requests with zero
GitHub approvals and resolved review threads; strict current-base success from
`Plan governance`, `Locked backend checks` and `Locked attention UI checks`;
linear history; and prohibits deletion and non-fast-forward updates.

The Apache-2.0 candidate remains a rights-changing human-review checkpoint. It
adds the official licence text, exact Cargo SPDX metadata and README licence
statement without a NOTICE file. P6c follows only after the owner inspects and
explicitly authorises the exact reviewed candidate's squash merge.

## Compact terminal checkpoints

| Checkpoint | Reviewed head | Merge commit | Tree | Result |
|---|---|---|---|---|
| P1 | `56a140acea93ecd8b71639c54ed0e098de55f515` | `8154ca4a5bc4c3aa0356cc74e3874544b4231296` | `36a3aac1c2ef6c3e5b4c66a96d3408e160e5a494` | PASS; PR #7 repaired the post-merge descendant fixture |
| P2 | `387e0300c7a7bca2136ab30748a47135c2b8847a` | `9cf9329f374f545eccca596fd9df8a451a48a065` | `49fe98d01d6f8969be9f0bad19f815f31719b11e` | PASS |
| P3 | `1f09350973632bb0fbdf9fd6c7e0103d6c21ed8b` | `9920006635c01cd266e6f2cd3a3546fe21747867` | `c844e629c88d363497eed5918e4f2553d941ac38` | PASS |
| P4a | `0854d10ae9b05b569d8d56113d43bd55d6eebdbe` | `87c8d42f1757c9656fbb010723b9e4bb1477dd69` | `0372ed9a79c1cc036ca306a0a12d34c47c0ad7ba` | PASS |
| P4b | `4cbe4a8fc6aa11e9d2f0c9720ccccc0bf157259f` | `4ecc30338c60bf66507558a68b96c65c34014c86` | `952a77d63477412722f0c2cf908a6ca4ae390c10` | Human-authorised PASS |
| P5a | `94024ae2a9bb266bbfb9102105728fbde99671f8` | `b8bda7877e63d73780f04dafe097fb7425cccc7a` | `365b4c870e5d50ff48c1117092679a4719ff3b83` | Exact CI `33872971463`; post-merge CI `33873290907`; landed 2026-09-04 |
| P5b | `30675a92bbf4091a5d09da4a60a62d9e774eaf92` | `d73f8417bbdce989c289ee3905da7ac28357964e` | `75374dc8a2799b1774fa894ad8d2e14427a6dc4e` | Exact CI `33885142461`; post-merge CI `33885489486`; landed 2026-09-05 |
| P6a | `ad2425e82cac1c8d125471090e0f3b51b6221246` | `bfb2104c55eaec6b01913c7ba5e97fba058bc77a` | `87d469076bf1e3a4a996958625c2434a96411a98` | Exact CI `33888079218`; post-merge CI `33888698360`; landed 2026-09-05 |

P5b's earlier repaired implementation review at
`3e12d6a59a1a4915c020fba0e6eca1eef676e801` (tree
`c63d016e52415c24e7721d5c28d0f1a493e7da87`, CI `33884226007`) remains
historical calibration evidence on PR #13, not the terminal reviewed head.

## P6b authority and evidence capsule

- Live `main` protection: explicitly authorised and applied on 2026-09-05 as
  active repository ruleset [22305031](https://github.com/robchristie/bokkie/rules/22305031),
  initial version `48733815`; authenticated ruleset GET and effective-branch
  GET returned the exact target, five rules and empty bypass list recorded in
  the current phase.
- Licence: Apache-2.0 was selected for a coherent candidate, but adding its
  rights grant remains human-held. Exact-head review and all checks do not
  authorise merge; the owner must inspect the candidate pull request and
  explicitly authorise its squash merge.

## Bounded residuals

- P6b licence merge remains the sole explicit human decision; live protection
  is active and verified without a bypass actor.
- P6c must observe the final licensed state and re-read the live ruleset before
  qualifying the exact resulting merged head.
- UI qualification remains fixture-only, not screen-reader certification,
  physical-GPU performance, production credentials, deployment or release.
