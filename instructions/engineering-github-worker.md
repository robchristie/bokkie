# Pagefold GitHub delivery supplement, revision 2

This explicit opt-in replaces only the base instructions' local-only push/PR
restriction for the registered Pagefold scope. Preserve existing personal and
repository guidance and installed rob-codex-workflow delivery skills. Do not
rewrite or copy their durable owners. Do not change global settings or seek
broader shell approvals. The operator has already authorised the bounded branch,
PR, review, CI and squash merge workflow; do not ask again for that authority.

Use bokkie_github for Git mutations and GitHub access; host credentials and .git
writes are unavailable to model shell commands. Each mutation takes operation,
arguments and the expected value from a fresh bokkie_snapshot. Supported worker
arguments are: prepare_branch {}, commit {paths:[relative files],message},
push {head:exact commit}, open_pr {head,title,body}, and
update_pr {pr,head,title,body,expected_text_digest}; status {pr:number} is read-only.
open_pr creates or finds the PR; it never edits existing text. Inspect text_applied
and next_action in its receipt. After repairs and the final review, refresh the
same PR with update_pr, using text_digest from open_pr/status as
expected_text_digest. A changed digest requires inspection, not blind overwrite.
Read back the receipt and require text_applied before claiming text was updated.
Prepare the branch before editing. Commit only intended files; run canonical
verification and inspect the complete diff. Source publication requires a clean
checkout descending from the currently observed main revision. The host adapter
cannot fetch/rebase: a changed base needs trusted workspace preparation and
renewed review. Never bypass the adapter or publish through shell commands.

Follow the normal independent exact-head review workflow. Register the actual
read-only review with bokkie_review; retain exact Git commit/tree artefacts and
actual validation observations. The reviewer remains independent and has no
mutation authority. Preserve the configured flagship/high review quality.
Open one PR for the bounded deliverable. Source repairs can be committed and
pushed to that same branch/PR. Include the PR number and limitations in the
submission; submit through the normal durable submission tool and cease. Bokkie
owns the final merge and acceptance after writer cessation, review and CI.

A pending delivery operation must be read back using reconcile
{operation_id:the returned identity}; it is not permission to retry the mutation.
Errors classified uncertain retain responsibility. Lost commit acknowledgements
may require trusted operator reconciliation; a matching message is not proof.
Status/CI waiting uses the existing bounded turn and durable supervisor scheduling,
not repeated fresh worker sessions. No deployment, release or access-policy change
is authorised by this supplement.
