# Bounded memory

Open **Settings → Memory** to inspect, add, correct or remove a remembered
preference, task outcome, decision or piece of operational knowledge. Each entry
shows its explicit or inferred provenance, supporting sources and current
revision. Supply a source reference and the relevant context or observation when
adding knowledge. There are no built-in personal preferences.

Corrections change the recalled content and retain its original provenance and
sources, with a visible **corrected by you** label. Removing an entry clears its
current content and excludes it from future recall. A source tombstone remains
so the same accepted outcome cannot recreate it. Idempotent command receipts
retain their original mutation results for lost-response reconciliation; removal
is removal from recall, not deletion of the authoritative task history or the
service's retained command history.

Saves and removals use the existing same-origin mutation token, an exact command
identity and the inspected entry revision. A lost acknowledgement retries the
same command. A revision conflict retains the entered correction and requires a
deliberate reload of current memory. Memory remains available when the model
runtime is unavailable, and editing memory makes no model call.

## Recall and authority

The first retrieval need is continuity after a workspace result leaves the recent
conversation. Selecting that task for a new conversation request can recall up
to three accepted workspace outcome excerpts. Only completed, accepted results
qualify; a worker's completion sentence or an incomplete result does not. Each
excerpt has a unique execution source and refers back to its authoritative task
result and delivery evidence. Re-reading a corrected or removed source never
replaces the correction or recreates the entry. There is no background sweep or
model-generated summary of all task history.

Conversation recall selects preferences, the selected task's memory and entries
matching up to four identifying words from the current request. It supplies at
most six entries, within the lesser of 4 KiB and one eighth of the accepted
request's context limit. This is a simple bounded relevance rule, with no
embedding service or additional model call. Large entries can be omitted when
they do not fit. Preferences are optional recall data; role-specific additional
instructions remain in the existing conversational role settings.

Memory is untrusted context. Current requests and instructions take precedence,
and maintained project knowledge stays with its project. Memory cannot grant
permissions, confirm a task, override current instructions, change execution
settings or silently alter runtime policy. Sources establish where recall came
from; they do not turn an inference into an independently verified fact.

## Persistence and bounds

Migration 20 adds the memory projection, source tombstones and atomic command
receipts. Entries have 2 KiB of content and one to four sources, each with a
256-byte reference and 512-byte context. The modest foundation permits 100 active
manually entered memories. The list uses 40-entry keyset pages. Accepted outcome
excerpts use at most 1,536 bytes of the saved result, without cutting a UTF-8
character. Task definitions, results and history are never rewritten by a memory
correction or removal.

The deterministic memory regressions cover CRUD, provenance, replay, stale
revision rejection, validation, bounded retrieval and paging, restart, accepted
outcome capture, correction/removal suppression and the unchanged task history.
HTTP and UI tests cover the mutation-token boundary, exact request retention,
service identity and the editor at desktop and narrow widths. Browser qualification uses the actual UI and Store in a private controller,
with no model call. The 19-check journey covers source/provenance, correction,
removal and browser closure/controller restart at 1440×1000 and 390×844. Its
committed-candidate attribution, opened captures and full report are retained
under `/tmp/bokkie-task-memory-evidence/browser-ba48/` and the delivery receipt.
It establishes these inputs, not production deployment or other devices.
