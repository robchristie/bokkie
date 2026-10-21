//! Per-Bot capability grants and the rules for setting them.
//!
//! Each Bot holds a set of [`Grant`]s from the catalogue. A Bot's thread
//! offers the model exactly the tools of the grants it started with (see
//! [`crate::tools`]), and a turn runs only while those equal the Bot's
//! grants: a change starts a fresh thread at the Bot's next turn.
//!
//! Setting grants follows three rules, checked by [`check_change`]:
//!
//! - **Availability.** A grant whose tools do not exist yet cannot be
//!   granted ([`available`]). Every grant in the catalogue has them today;
//!   `web.search` brings Codex's own web search rather than a Bokkie tool,
//!   which its threads turn on
//!   ([`crate::codex::config::thread_overrides`]).
//! - **Who grants what.** An admin may grant anything available; a member
//!   may add only `notes`, `seen` and `notify` ([`member_may_grant`]), and
//!   may remove any grant.
//! - **The taint rule.** A Bot granted `web.search` or `web.fetch` reads
//!   untrusted input, and may never also hold a privileged grant
//!   ([`taint_conflict`]). No privileged grant exists yet; `host.*` and
//!   `github.*` will be.

use std::collections::BTreeSet;

use bokkie_protocol::api::{Grant, Role};

use crate::error::{Failure, Result};

/// A Bot's grants, in catalogue order.
pub type GrantSet = BTreeSet<Grant>;

/// The trust class of a grant, for the taint rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Reads or writes only the Bot's own data, or tells its owner.
    Ordinary,
    /// Brings content from outside the household into the conversation,
    /// where it can carry instructions.
    UntrustedInput,
    /// Acts on something beyond the conversation that untrusted content
    /// must never steer, such as a host or a repository.
    Privileged,
}

/// The class of each grant in the catalogue.
pub fn class(grant: Grant) -> Class {
    match grant {
        Grant::Notes | Grant::Seen | Grant::Notify => Class::Ordinary,
        Grant::WebSearch | Grant::WebFetch => Class::UntrustedInput,
    }
}

/// Whether the gateway can grant this yet: its tools exist. Every grant in
/// the catalogue can be granted today; a grant added before its tools must
/// be refused here.
pub fn available(grant: Grant) -> bool {
    match grant {
        Grant::Notes | Grant::Seen | Grant::Notify | Grant::WebSearch | Grant::WebFetch => true,
    }
}

/// Whether a member may add this grant to their own Bot. Outward-facing
/// grants are admin-only.
pub fn member_may_grant(grant: Grant) -> bool {
    matches!(grant, Grant::Notes | Grant::Seen | Grant::Notify)
}

/// The first untrusted-input grant and privileged grant held together, if
/// any: a set of grants, each with its class, breaks the taint rule when
/// it has both.
pub fn taint_conflict<G: Copy>(grants: impl IntoIterator<Item = (G, Class)>) -> Option<(G, G)> {
    let mut untrusted = None;
    let mut privileged = None;
    for (grant, class) in grants {
        match class {
            Class::UntrustedInput => untrusted = untrusted.or(Some(grant)),
            Class::Privileged => privileged = privileged.or(Some(grant)),
            Class::Ordinary => {}
        }
    }
    untrusted.zip(privileged)
}

/// Checks a request to replace a Bot's grants `current` with `requested`,
/// made by an account with `role`, and returns the new set.
pub fn check_change(role: Role, current: &GrantSet, requested: &[Grant]) -> Result<GrantSet> {
    check_change_classed(role, current, requested, class)
}

/// [`check_change`], with each grant's class from `class`, so that a test
/// can show the taint rule refusing a set before any privileged grant
/// exists.
fn check_change_classed(
    role: Role,
    current: &GrantSet,
    requested: &[Grant],
    class: impl Fn(Grant) -> Class,
) -> Result<GrantSet> {
    let requested: GrantSet = requested.iter().copied().collect();
    for grant in requested.difference(current) {
        if role != Role::Admin && !member_may_grant(*grant) {
            return Err(Failure::forbidden(format!(
                "Only an admin can grant {grant}"
            )));
        }
        if !available(*grant) {
            return Err(Failure::invalid(format!("{grant} is not available yet")));
        }
    }
    if let Some((untrusted, privileged)) =
        taint_conflict(requested.iter().map(|grant| (*grant, class(*grant))))
    {
        return Err(Failure::invalid(format!(
            "A Bot granted {untrusted} reads untrusted input, so it cannot also hold \
             {privileged}"
        )));
    }
    Ok(requested)
}

/// The grants as the database stores them: names joined by commas.
pub fn to_stored(grants: &GrantSet) -> String {
    grants
        .iter()
        .map(|g| g.name())
        .collect::<Vec<_>>()
        .join(",")
}

/// The grants from their stored form. A name the catalogue no longer has is
/// dropped: it grants nothing.
pub fn from_stored(stored: &str) -> GrantSet {
    stored
        .split(',')
        .filter(|name| !name.is_empty())
        .filter_map(|name| {
            let grant = Grant::from_name(name);
            if grant.is_none() {
                tracing::warn!(name, "ignoring a stored grant the catalogue does not have");
            }
            grant
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bokkie_protocol::api::ErrorCode;

    #[test]
    fn web_grants_are_untrusted_input_and_none_is_privileged_yet() {
        for grant in Grant::ALL {
            let expected = match grant {
                Grant::WebSearch | Grant::WebFetch => Class::UntrustedInput,
                _ => Class::Ordinary,
            };
            assert_eq!(class(grant), expected, "{grant}");
        }
    }

    #[test]
    fn the_taint_rule_refuses_untrusted_input_with_privilege() {
        use Class::*;
        assert_eq!(taint_conflict::<&str>([]), None);
        assert_eq!(
            taint_conflict([("notes", Ordinary), ("web.search", UntrustedInput)]),
            None
        );
        assert_eq!(
            taint_conflict([("notes", Ordinary), ("host.run", Privileged)]),
            None
        );
        assert_eq!(
            taint_conflict([
                ("web.fetch", UntrustedInput),
                ("notes", Ordinary),
                ("github.push", Privileged),
                ("web.search", UntrustedInput),
            ]),
            Some(("web.fetch", "github.push"))
        );
        // Every catalogue combination passes today: nothing is privileged.
        for bits in 0..(1u32 << Grant::ALL.len()) {
            let set: Vec<Grant> = Grant::ALL
                .into_iter()
                .enumerate()
                .filter(|(i, _)| bits & (1 << i) != 0)
                .map(|(_, g)| g)
                .collect();
            assert_eq!(taint_conflict(set.iter().map(|g| (*g, class(*g)))), None);
        }
    }

    #[test]
    fn members_may_add_only_notes_seen_and_notify() {
        let granted: Vec<Grant> = Grant::ALL
            .into_iter()
            .filter(|g| member_may_grant(*g))
            .collect();
        assert_eq!(granted, [Grant::Notes, Grant::Seen, Grant::Notify]);

        let none = GrantSet::new();
        let notes = check_change(Role::Member, &none, &[Grant::Notes]).unwrap();
        assert_eq!(notes, [Grant::Notes].into());
        for outward in [Grant::WebSearch, Grant::WebFetch] {
            let refused = check_change(Role::Member, &none, &[outward]).unwrap_err();
            assert_eq!(refused.code, ErrorCode::Forbidden, "{outward}");
        }
        // Removing an admin's grant is allowed, and keeping one is not
        // adding it.
        let held: GrantSet = [Grant::Notes, Grant::WebSearch].into();
        assert_eq!(
            check_change(Role::Member, &held, &[Grant::WebSearch]).unwrap(),
            [Grant::WebSearch].into()
        );
        assert!(check_change(Role::Member, &held, &[]).unwrap().is_empty());
    }

    #[test]
    fn every_grant_in_the_catalogue_can_be_granted() {
        for grant in Grant::ALL {
            assert!(available(grant), "{grant}");
        }
        let none = GrantSet::new();
        assert_eq!(
            check_change(Role::Member, &none, &[Grant::Seen, Grant::Notes]).unwrap(),
            [Grant::Notes, Grant::Seen].into()
        );
        assert_eq!(
            check_change(Role::Member, &none, &[Grant::Notify]).unwrap(),
            [Grant::Notify].into()
        );
        // An admin may grant the whole catalogue: both web grants are
        // untrusted input, and none is privileged.
        assert_eq!(
            check_change(Role::Admin, &none, &Grant::ALL).unwrap(),
            Grant::ALL.into()
        );
        assert_eq!(
            check_change(Role::Admin, &none, &[Grant::WebSearch]).unwrap(),
            [Grant::WebSearch].into()
        );
        // Duplicates collapse.
        assert_eq!(
            check_change(Role::Admin, &none, &[Grant::Notes, Grant::Notes]).unwrap(),
            [Grant::Notes].into()
        );
    }

    #[test]
    fn a_web_grant_is_refused_beside_a_privileged_one() {
        // No grant is privileged yet, so `notify` stands in for one.
        let pretend = |grant| match grant {
            Grant::Notify => Class::Privileged,
            other => class(other),
        };
        let none = GrantSet::new();
        for web in [Grant::WebSearch, Grant::WebFetch] {
            let refused = check_change_classed(Role::Admin, &none, &[Grant::Notify, web], pretend)
                .unwrap_err();
            assert_eq!(refused.code, ErrorCode::InvalidRequest, "{web}");
            assert_eq!(
                refused.message,
                format!("A Bot granted {web} reads untrusted input, so it cannot also hold notify")
            );
            // Already holding the privileged grant makes no difference.
            let held: GrantSet = [Grant::Notify].into();
            assert!(
                check_change_classed(Role::Admin, &held, &[Grant::Notify, web], pretend).is_err()
            );
            assert_eq!(
                check_change_classed(Role::Admin, &none, &[web, Grant::Notes], pretend).unwrap(),
                [web, Grant::Notes].into()
            );
        }
    }

    #[test]
    fn grants_are_stored_by_name() {
        let set: GrantSet = [Grant::WebFetch, Grant::Notes].into();
        assert_eq!(to_stored(&set), "notes,web.fetch");
        assert_eq!(from_stored("notes,web.fetch"), set);
        assert_eq!(from_stored(""), GrantSet::new());
        assert_eq!(from_stored("notes,host.run"), [Grant::Notes].into());
    }
}
