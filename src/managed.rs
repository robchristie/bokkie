//! Local note execution: rendering is deterministic and happens outside SQLite.
use crate::{ManagedTaskDefinition, Store, StoreError};

pub fn render_local_note(definition: &ManagedTaskDefinition) -> String {
    let mut text = definition.instructions.clone();
    if !definition.context_refs.is_empty() {
        text.push_str("\n\nContext references:\n");
        text.push_str(&definition.context_refs.join("\n"));
    }
    text.chars()
        .take(definition.max_output_chars as usize)
        .collect()
}

/// Execute at most one admitted note. Stable obligation identity deduplicates
/// the result; a crash before completion is recovered by the kernel lease.
pub fn run_one_note(store: &mut Store, now: i64) -> Result<bool, StoreError> {
    let Some(claim) = store.claim_due_notes(now, 30, 1)?.pop() else {
        return Ok(false);
    };
    let definition = store.managed_note_definition(&claim.obligation_id)?;
    let result = render_local_note(&definition.definition);
    store.complete_managed_note(&claim, &result, now)?;
    Ok(true)
}
